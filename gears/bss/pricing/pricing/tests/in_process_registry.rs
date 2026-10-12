//! Products' in-process registry reads its own database through `Db::conn()`, which toolkit-db
//! refuses on a task that is inside a transaction (`ConnRequestedInsideTx`). The approval
//! subjects read SKUs inside the unit's transaction (D-408; D-402's dated metering reads), so the
//! doors must not hand the registry their transaction's task. The double here refuses exactly as
//! Products does, so a submit, a publish or a vote that reads a SKU on the transaction's task
//! answers 503 instead of recording its unit.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod plan_support;
use bss_pricing::infra::storage::repo::{price_book_entry_repo, price_repo};
use bss_products_sdk::{
    ReferenceRegistryV1,
    models::{ReferenceKind, ReferenceState, ReservationReceipt, Sku, SkuType, SkuVersion},
};
use plan_support::entry_support::policy_support;
use plan_support::{Catalog, Fixture, book, id_of, plan, policy_entry as entry, scope};
use serde_json::{Value, json};
use std::sync::Arc;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// The catalog double behind Products' own first step: a connection to its own database, refused
/// inside the caller's transaction as `LocalReferenceRegistry` is.
struct InProcess {
    catalog: Arc<Catalog>,
    db: toolkit_db::DBProvider<toolkit_db::DbError>,
    /// Holds `db`'s temporary directory for the registry's life.
    _dsn: plan_support::entry_support::TestDsn,
}
impl InProcess {
    fn connect(&self) -> Result<(), CanonicalError> {
        self.db
            .conn()
            .map(|_| ())
            .map_err(|e| CanonicalError::internal(e.to_string()).create())
    }
}
#[async_trait::async_trait]
impl ReferenceRegistryV1 for InProcess {
    async fn reserve(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        sku: Uuid,
        kind: ReferenceKind,
        ref_id: Uuid,
    ) -> Result<ReservationReceipt, CanonicalError> {
        self.connect()?;
        self.catalog.reserve(ctx, tenant, sku, kind, ref_id).await
    }
    async fn confirm(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
    ) -> Result<(), CanonicalError> {
        self.connect()?;
        self.catalog.confirm(ctx, tenant, id).await
    }
    async fn release(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
    ) -> Result<(), CanonicalError> {
        self.connect()?;
        self.catalog.release(ctx, tenant, id).await
    }
    async fn states(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        ids: &[Uuid],
    ) -> Result<Vec<(Uuid, ReferenceState)>, CanonicalError> {
        self.connect()?;
        self.catalog.states(ctx, tenant, ids).await
    }
    async fn sku_for_write(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
    ) -> Result<Sku, CanonicalError> {
        self.connect()?;
        self.catalog.sku_for_write(ctx, tenant, id).await
    }
    async fn sku_version_as_of(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
        date: time::Date,
    ) -> Result<Option<SkuVersion>, CanonicalError> {
        self.connect()?;
        self.catalog.sku_version_as_of(ctx, tenant, id, date).await
    }
}

async fn setup() -> (Fixture, Arc<Catalog>) {
    let catalog = Arc::new(Catalog::default());
    let (db, _, _, dsn) = plan_support::entry_support::test_db().await;
    let f = Fixture::new(Arc::new(InProcess {
        catalog: catalog.clone(),
        db,
        _dsn: dsn,
    }))
    .await;
    (f, catalog)
}
async fn quorum(f: &Fixture, kind: &str, quorum: u32) {
    let (_, _, tag) = f
        .call("GET", "/approval-policy", json!({}), None, None)
        .await;
    let (s, b, _) = f
        .call(
            "PUT",
            "/approval-policy",
            json!({"kind":kind,"quorum":quorum}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
}
async fn draft(f: &Fixture, entry: &str, from: &str, key: &str) -> Value {
    let (s, b, _) = f
        .call(
            "POST",
            &format!("/price-book-entries/{entry}/prices"),
            json!({"price":{"rate":"0.10"},"eligibility":"all","effective_from":from}),
            None,
            Some(key),
        )
        .await;
    assert_eq!(s, 201, "{b}");
    b["items"][0].clone()
}

/// A `prices` unit reads each entry SKU (its descriptors, D-408) and a usage chain's dated
/// metering (D-402) inside its transaction: publish-changes at quorum 0, and a submit then an
/// approving vote at quorum 1, record and apply their units.
#[tokio::test]
async fn a_prices_unit_is_recorded_and_applied_through_an_in_process_registry() {
    let (f, catalog) = setup().await;
    let book = book(&f, "eur").await;
    let sku = catalog.sku(SkuType::Usage);
    let (s, e, _) = f
        .call(
            "POST",
            &format!("/price-books/{book}/entries"),
            json!({"usage_rating_policy":policy_support::input(),"sku_id":sku,"model":"per_unit"}),
            None,
            Some("entry"),
        )
        .await;
    assert_eq!(s, 201, "{e}");
    let e = e["id"].as_str().unwrap().to_owned();
    quorum(&f, "prices", 0).await;
    draft(&f, &e, "2031-03-01", "first").await;
    let (s, b, _) = f
        .call(
            "POST",
            &format!("/price-books/{book}/publish-changes"),
            json!({}),
            None,
            Some("publish"),
        )
        .await;
    assert_eq!(s, 201, "{b}");
    assert_eq!(b["applied"], true, "{b}");
    quorum(&f, "prices", 1).await;
    let price = draft(&f, &e, "2031-06-01", "second").await;
    let (s, receipt, _) = f
        .call(
            "POST",
            &format!("/prices/{}/submit", price["id"].as_str().unwrap()),
            json!({}),
            None,
            Some("submit"),
        )
        .await;
    assert_eq!(s, 201, "{receipt}");
    let (s, b, _) = f
        .call_as(
            &f.user(),
            "POST",
            &format!(
                "/approval-units/{}/approve",
                receipt["unit"]["id"].as_str().unwrap()
            ),
            json!({"generation":1}),
            None,
            Some("approve"),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["outcome"], "applied", "{b}");
}

/// A `plan_revision` unit judges the revision with fresh SKU reads at submit and again at apply,
/// inside its transaction: the submit records the unit and the approving vote publishes it.
#[tokio::test]
async fn a_plan_revision_is_submitted_and_applied_through_an_in_process_registry() {
    let (f, catalog) = setup().await;
    let eur = book(&f, "eur").await;
    let (created, revision) = plan(&f, "pro", eur).await;
    let sku = catalog.sku(SkuType::Usage);
    let priced = entry(&f, eur, sku, "usage", None).await;
    let conn = f.db.conn().unwrap();
    let stored = price_book_entry_repo::find(&conn, &scope(&f), f.ctx.subject_tenant_id(), priced)
        .await
        .unwrap()
        .unwrap();
    let mut approved = plan_support::entry_support::price(&stored);
    approved.state = "approved".into();
    approved.effective_from = time::Date::parse(
        "2020-01-01",
        &time::format_description::well_known::Iso8601::DATE,
    )
    .unwrap();
    price_repo::insert(&conn, &scope(&f), approved)
        .await
        .unwrap();
    let (s, b, _) = f
        .call(
            "POST",
            &format!("/plan-revisions/{revision}/items"),
            json!({"sku_id":sku,"price_book_entry_id":priced}),
            None,
            Some("item"),
        )
        .await;
    assert_eq!(s, 201, "{b}");
    quorum(&f, "plan_revision", 1).await;
    let (s, receipt, _) = f
        .call(
            "POST",
            &format!("/plan-revisions/{revision}/submit"),
            json!({}),
            None,
            Some("submit"),
        )
        .await;
    assert_eq!(s, 201, "{receipt}");
    assert_eq!(receipt["revision"]["state"], "pending", "{receipt}");
    let (s, b, _) = f
        .call_as(
            &f.user(),
            "POST",
            &format!(
                "/approval-units/{}/approve",
                receipt["unit"]["id"].as_str().unwrap()
            ),
            json!({"generation":1}),
            None,
            Some("approve"),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["outcome"], "applied", "{b}");
    let (s, plan_now, _) = f
        .call(
            "GET",
            &format!("/plans/{}", id_of(&created["id"])),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{plan_now}");
    assert_eq!(plan_now["published_rev"], 1);
}

/// `GET /resolve` reads each SKU version as of the date through the in-process registry (D-421):
/// its read transaction is over by then, so Products' own connection is not refused.
#[tokio::test]
async fn a_revision_resolves_through_an_in_process_registry() {
    let (f, catalog) = setup().await;
    let eur = book(&f, "eur").await;
    let sku = catalog.sku(SkuType::Recurring);
    let mut content = catalog.content(sku);
    content.gl_code = Some("4000".into());
    catalog.version(sku, 1, "2020-01-01", content);
    let priced = plan_support::entry_in(&f, eur, sku, "recurring", Some("month"), "flat").await;
    let conn = f.db.conn().unwrap();
    let stored = price_book_entry_repo::find(&conn, &scope(&f), f.ctx.subject_tenant_id(), priced)
        .await
        .unwrap()
        .unwrap();
    let mut approved = plan_support::entry_support::price(&stored);
    approved.state = "approved".into();
    // D-427: the recurring entry is `flat`, so its price's money is a flat amount.
    approved.price_json = json!({"amount":"30.00"});
    approved.min_fee = None;
    approved.effective_from = time::Date::parse(
        "2020-01-01",
        &time::format_description::well_known::Iso8601::DATE,
    )
    .unwrap();
    let price = price_repo::insert(&conn, &scope(&f), approved)
        .await
        .unwrap()
        .id;
    let (created, revision) = plan(&f, "pro", eur).await;
    plan_support::item(&f, revision, sku, Some(priced), "paid").await;
    plan_support::publish(&f, id_of(&created["id"]), revision).await;
    let (s, b, _) = f
        .call(
            "GET",
            &format!("/resolve?plan_revision_id={revision}&date=2026-10-05&pins={price}"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    let item = &b["items"][0];
    assert_eq!(
        item["sku_version"],
        json!({"published_version": 1, "effective_from": "2020-01-01"}),
        "{b}"
    );
    assert_eq!(item["gl_code"], json!({"value":"4000","source":"sku"}));
    assert_eq!(item["chains"][0]["binding"]["price_id"], json!(price));
}

/// Products in process over pricing's OWN one-connection pool (the fixture database's): its
/// dated read needs that connection, so it answers only once the door has given it back. The
/// detached registry reads on a task of its own, which the transaction guard of the double above
/// cannot see; this one sees a door that still holds its read transaction while it reads Products:
/// the read waits on the door itself, gives up after two seconds and answers as an unavailable
/// registry.
struct SharedPool {
    catalog: Arc<Catalog>,
    db: std::sync::OnceLock<toolkit_db::DBProvider<toolkit_db::DbError>>,
}
impl SharedPool {
    async fn wait_for_the_connection(&self, tenant: Uuid) -> Result<(), CanonicalError> {
        let db = self.db.get().unwrap();
        let conn = db
            .conn()
            .map_err(|e| CanonicalError::internal(e.to_string()).create())?;
        let read = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            bss_pricing::infra::storage::repo::plan_repo::list(
                &conn,
                &toolkit_db::secure::AccessScope::for_tenant(tenant),
                tenant,
            ),
        )
        .await;
        match read {
            Ok(Ok(_)) => Ok(()),
            _ => Err(CanonicalError::service_unavailable().create()),
        }
    }
}
#[async_trait::async_trait]
impl ReferenceRegistryV1 for SharedPool {
    async fn reserve(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        sku: Uuid,
        kind: ReferenceKind,
        ref_id: Uuid,
    ) -> Result<ReservationReceipt, CanonicalError> {
        self.catalog.reserve(ctx, tenant, sku, kind, ref_id).await
    }
    async fn confirm(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
    ) -> Result<(), CanonicalError> {
        self.catalog.confirm(ctx, tenant, id).await
    }
    async fn release(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
    ) -> Result<(), CanonicalError> {
        self.catalog.release(ctx, tenant, id).await
    }
    async fn states(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        ids: &[Uuid],
    ) -> Result<Vec<(Uuid, ReferenceState)>, CanonicalError> {
        self.catalog.states(ctx, tenant, ids).await
    }
    async fn sku_for_write(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
    ) -> Result<Sku, CanonicalError> {
        self.catalog.sku_for_write(ctx, tenant, id).await
    }
    async fn sku_version_as_of(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        id: Uuid,
        date: time::Date,
    ) -> Result<Option<SkuVersion>, CanonicalError> {
        self.wait_for_the_connection(tenant).await?;
        self.catalog.sku_version_as_of(ctx, tenant, id, date).await
    }
}

/// D-421 and the brief's "OUTSIDE the transaction": resolve reads Products only after its one
/// read transaction has ended and given its connection back.
#[tokio::test]
async fn resolve_reads_products_only_after_its_read_transaction_gave_its_connection_back() {
    let catalog = Arc::new(Catalog::default());
    let shared = Arc::new(SharedPool {
        catalog: catalog.clone(),
        db: std::sync::OnceLock::new(),
    });
    let f = Fixture::new(shared.clone()).await;
    assert!(shared.db.set(f.db.clone()).is_ok());
    let eur = book(&f, "eur").await;
    let sku = catalog.sku(SkuType::Recurring);
    catalog.version(sku, 7, "2020-01-01", catalog.content(sku));
    let priced = entry(&f, eur, sku, "recurring", Some("month")).await;
    let (created, revision) = plan(&f, "pro", eur).await;
    plan_support::item(&f, revision, sku, Some(priced), "paid").await;
    plan_support::publish(&f, id_of(&created["id"]), revision).await;
    let (s, b, _) = f
        .call(
            "GET",
            &format!("/resolve?plan_revision_id={revision}&date=2026-10-05"),
            json!({}),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["items"][0]["sku_version"]["published_version"], 7, "{b}");
}
