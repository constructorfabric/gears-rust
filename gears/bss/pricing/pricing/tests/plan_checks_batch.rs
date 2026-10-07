//! The checks read their context as a set, and many revisions are checked in one read
//! (phase 9 run 9.7, D-482, P-D-245).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::too_many_lines)]
mod plan_support;
use bss_products_sdk::ReferenceRegistryV1;
use bss_products_sdk::models::{
    ReferenceKind, ReferenceState, ReservationReceipt, Sku, SkuType, SkuVersion,
};
use plan_support::{
    Catalog, Fixture, book, entry, entry_support, holding, item, plan, request, setup,
};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

fn sql(recorder: &toolkit_db::test_support::QueryRecorder) -> Vec<String> {
    recorder
        .events()
        .into_iter()
        .filter(|q| {
            q.table
                .as_deref()
                .is_some_and(|t| t.starts_with("pricing_"))
        })
        .map(|q| q.sql)
        .collect()
}
async fn recorded() -> (
    Fixture,
    Arc<Catalog>,
    toolkit_db::test_support::QueryRecorder,
) {
    let (db, recorder, tenant, dsn) = entry_support::recorded_db().await;
    let catalog = Arc::new(Catalog::default());
    let f = Fixture::on(db, tenant, dsn, catalog.clone()).await;
    (f, catalog, recorder)
}
async fn get(f: &Fixture, path: &str) -> (u16, Value) {
    let (status, body, _) = f.call("GET", path, json!({}), None, None).await;
    (status, body)
}
async fn draft_with_entries(f: &Fixture, catalog: &Catalog, code: &str, n: usize) -> Uuid {
    let book_id = book(f, code).await;
    let (_, rev) = plan(f, code, book_id).await;
    for i in 0..n {
        let sku = catalog.sku(SkuType::Recurring);
        let priced = entry(f, book_id, sku, "recurring", Some("month")).await;
        item(f, rev, sku, Some(priced), "paid").await;
        let _ = i;
    }
    rev
}
fn batch_path(ids: &[Uuid]) -> String {
    let list = ids
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    format!("/plan-revisions/checks?revision_ids={list}")
}

/// A registry reached through `reference_registry::resolve`: it counts `skus_for_write` itself
/// and `sku_for_write` only when that method is the one called.
struct Counting {
    inner: Arc<Catalog>,
    sku_for_write: AtomicUsize,
    skus_for_write: AtomicUsize,
}
impl Counting {
    fn new(inner: Arc<Catalog>) -> Self {
        Self {
            inner,
            sku_for_write: AtomicUsize::new(0),
            skus_for_write: AtomicUsize::new(0),
        }
    }
}
#[async_trait::async_trait]
impl ReferenceRegistryV1 for Counting {
    async fn reserve(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        sku: Uuid,
        kind: ReferenceKind,
        ref_id: Uuid,
    ) -> Result<ReservationReceipt, CanonicalError> {
        self.inner.reserve(ctx, tenant, sku, kind, ref_id).await
    }
    async fn confirm(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
    ) -> Result<(), CanonicalError> {
        self.inner.confirm(ctx, tenant, id).await
    }
    async fn release(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
    ) -> Result<(), CanonicalError> {
        self.inner.release(ctx, tenant, id).await
    }
    async fn states(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        ids: &[Uuid],
    ) -> Result<Vec<(Uuid, ReferenceState)>, CanonicalError> {
        self.inner.states(ctx, tenant, ids).await
    }
    async fn sku_for_write(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
    ) -> Result<Sku, CanonicalError> {
        self.sku_for_write.fetch_add(1, Ordering::SeqCst);
        self.inner.sku_for_write(ctx, tenant, id).await
    }
    async fn skus_for_write(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        ids: &[Uuid],
    ) -> Result<Vec<Sku>, CanonicalError> {
        self.skus_for_write.fetch_add(1, Ordering::SeqCst);
        let mut seen = std::collections::BTreeSet::new();
        let mut found = Vec::new();
        for id in ids {
            if !seen.insert(*id) {
                continue;
            }
            match self.inner.sku_for_write(ctx, tenant, *id).await {
                Ok(sku) => found.push(sku),
                Err(error) if error.status_code() == 404 => {}
                Err(error) => return Err(error),
            }
        }
        Ok(found)
    }
    async fn sku_version_as_of(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
        date: time::Date,
    ) -> Result<Option<SkuVersion>, CanonicalError> {
        self.inner.sku_version_as_of(ctx, tenant, id, date).await
    }
}

/// A missing `Detached` override would call `sku_for_write` per id. The checks read calls
/// `skus_for_write` once, and an all-missing batch calls Products not at all.
#[tokio::test]
async fn the_checks_read_calls_skus_for_write_once_through_resolve() {
    let catalog = Arc::new(Catalog::default());
    let counting = Arc::new(Counting::new(catalog.clone()));
    let f = Fixture::new(counting.clone()).await;
    let missing = Uuid::now_v7();
    let (status, body) = get(
        &f,
        &format!("/plan-revisions/checks?revision_ids={missing}"),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["missing"], json!([missing.to_string()]));
    assert!(body["items"].as_array().unwrap().is_empty());
    assert_eq!(counting.skus_for_write.load(Ordering::SeqCst), 0);
    assert_eq!(counting.sku_for_write.load(Ordering::SeqCst), 0);
    let rev = draft_with_entries(&f, &catalog, "pro", 1).await;
    let (status, body) = get(&f, &format!("/plan-revisions/{rev}/checks")).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(counting.skus_for_write.load(Ordering::SeqCst), 1);
    assert_eq!(counting.sku_for_write.load(Ordering::SeqCst), 0);
}

/// The single read and the batch read answer the same checks, and a foreign id or one outside a
/// narrowed plan-read scope is `missing`.
#[tokio::test]
async fn the_batch_matches_the_single_read_and_names_what_is_missing() {
    let (f, catalog) = setup().await;
    let a = draft_with_entries(&f, &catalog, "alpha", 1).await;
    let b = draft_with_entries(&f, &catalog, "beta", 2).await;
    let (status, left) = get(&f, &format!("/plan-revisions/{a}/checks")).await;
    assert_eq!(status, 200, "{left}");
    let (status, right) = get(&f, &format!("/plan-revisions/{b}/checks")).await;
    assert_eq!(status, 200, "{right}");
    let (status, batch) = get(&f, &batch_path(&[b, a])).await;
    assert_eq!(status, 200, "{batch}");
    assert!(batch["missing"].as_array().unwrap().is_empty(), "{batch}");
    let items = batch["items"].as_array().unwrap();
    assert_eq!(items[0]["revision_id"], b.to_string());
    assert_eq!(items[0]["checks"], right);
    assert_eq!(items[1]["revision_id"], a.to_string());
    assert_eq!(items[1]["checks"], left);
    let foreign = Uuid::now_v7();
    let (status, batch) = get(&f, &batch_path(&[a, foreign])).await;
    assert_eq!(status, 200, "{batch}");
    assert_eq!(batch["items"].as_array().unwrap().len(), 1);
    assert_eq!(batch["missing"], json!([foreign.to_string()]));
    let narrowed = entry_support::production(f.state.clone()).layer(axum::Extension(
        authz_resolver_sdk::PolicyEnforcer::new(Arc::new(Narrowed {
            tenant: f.ctx.subject_tenant_id(),
            revisions: vec![a],
        })),
    ));
    let (status, body, _) = request(
        &narrowed,
        &f.ctx,
        "GET",
        &batch_path(&[a, b]),
        json!({}),
        None,
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["items"][0]["revision_id"], a.to_string());
    assert_eq!(body["items"][0]["checks"], left);
    assert_eq!(body["missing"], json!([b.to_string()]));
    let (status, one) = get(&f, &format!("/plan-revisions/{a}")).await;
    assert_eq!(status, 200, "{one}");
    assert_eq!(one["id"], a.to_string());
    assert!(one.get("missing").is_none(), "{one}");
}

struct Narrowed {
    tenant: Uuid,
    revisions: Vec<Uuid>,
}
#[async_trait::async_trait]
impl authz_resolver_sdk::AuthZResolverApi for Narrowed {
    async fn evaluate(
        &self,
        _: toolkit_security::PlatformSecurityContext,
        request: authz_resolver_sdk::EvaluationRequest,
    ) -> Result<authz_resolver_sdk::EvaluationResponse, CanonicalError> {
        use authz_resolver_sdk::*;
        let mut predicates = vec![Predicate::In(InPredicate::new(
            toolkit_security::pep_properties::OWNER_TENANT_ID,
            vec![self.tenant],
        ))];
        if request.resource.resource_type == "gts.cf.bss.pricing.plan.v1~"
            && request.action.name == "read"
        {
            predicates.push(Predicate::In(InPredicate::new(
                toolkit_security::pep_properties::RESOURCE_ID,
                self.revisions.clone(),
            )));
        }
        Ok(EvaluationResponse {
            decision: true,
            context: EvaluationResponseContext {
                constraints: vec![Constraint { predicates }],
                deny_reason: None,
            },
        })
    }
}

/// Every refusal of the batch query, the grant, Products' 403 and the registry's 503.
#[tokio::test]
async fn the_batch_refuses_a_bad_query_a_missing_grant_and_an_unanswered_registry() {
    let (f, catalog) = setup().await;
    let rev = draft_with_entries(&f, &catalog, "pro", 1).await;
    let id = rev.to_string();
    for path in [
        "/plan-revisions/checks",
        "/plan-revisions/checks?revision_ids=",
        "/plan-revisions/checks?revision_ids=not-a-uuid",
        &format!("/plan-revisions/checks?revision_ids={id},{id}"),
        &format!("/plan-revisions/checks?revision_ids={id}&revision_ids={id}"),
        &format!("/plan-revisions/checks?revision_ids={id}&other=1"),
    ] {
        let (status, body) = get(&f, path).await;
        assert_eq!(status, 400, "{path}: {body}");
        assert!(body.to_string().contains("QUERY_INVALID"), "{path}: {body}");
    }
    let mut many = Vec::new();
    for _ in 0..51 {
        many.push(Uuid::new_v4());
    }
    let (status, body) = get(&f, &batch_path(&many)).await;
    assert_eq!(status, 400, "{body}");
    let (status, body, _) = f
        .call_as(
            &holding(&f, "price:read"),
            "GET",
            &batch_path(&[rev]),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(status, 403, "{body}");
    catalog.readers(std::iter::empty::<Uuid>());
    let (status, body) = get(&f, &batch_path(&[rev])).await;
    assert_eq!(status, 403, "{body}");
    assert!(body.to_string().contains("SKU_READ_DENIED"), "{body}");
    catalog.readers([f.ctx.subject_id()]);
    catalog.down.store(true, Ordering::SeqCst);
    let (status, body) = get(&f, &batch_path(&[rev])).await;
    assert_eq!(status, 503, "{body}");
    assert!(body.to_string().contains("REGISTRY_UNAVAILABLE"), "{body}");
}

/// D-482: the checks read is the same statements for one revision and for a wide batch, on a
/// connection, not under a serializable transaction.
#[tokio::test]
async fn the_checks_read_is_eleven_statements_whatever_the_set() {
    let (f, catalog, recorder) = recorded().await;
    let one = draft_with_entries(&f, &catalog, "one", 1).await;
    recorder.clear();
    let (status, body) = get(&f, &format!("/plan-revisions/{one}/checks")).await;
    assert_eq!(status, 200, "{body}");
    let single = sql(&recorder);
    assert!(
        recorder.events().iter().all(|q| !q.in_tx),
        "the checks read is not a transaction: {:?}",
        recorder
            .events()
            .iter()
            .map(|q| q.in_tx)
            .collect::<Vec<_>>()
    );
    let mut narrow_ids = Vec::new();
    for i in 0..5 {
        narrow_ids.push(draft_with_entries(&f, &catalog, &format!("n{i}"), 1).await);
    }
    recorder.clear();
    let (status, body) = get(&f, &batch_path(&narrow_ids)).await;
    assert_eq!(status, 200, "{body}");
    let five = sql(&recorder);
    let mut wide = Vec::new();
    for i in 0..50 {
        wide.push(draft_with_entries(&f, &catalog, &format!("w{i:02}"), 20).await);
    }
    recorder.clear();
    let (status, body) = get(&f, &batch_path(&wide)).await;
    assert_eq!(status, 200, "{body}");
    let fifty = sql(&recorder);
    assert_eq!(single.len(), 11, "{single:#?}");
    assert_eq!(single, five, "one revision and five");
    assert_eq!(
        five, fifty,
        "five revisions of one entry and fifty of twenty"
    );
    assert!(recorder.events().iter().all(|q| !q.in_tx));
}
