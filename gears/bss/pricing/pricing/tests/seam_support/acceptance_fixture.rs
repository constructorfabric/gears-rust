//! Real acceptance service fixture; successful receipts come only from the command.
#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]
use crate::{plan_support as p, seam_support as s};
use bss_pricing::{
    api::{
        pricing_acceptance::PricingAcceptanceProvider, pricing_read::PricingReadProvider,
        sellability::SellabilityProvider,
    },
    config::SellerHoldPolicy,
    infra::{clock::Clock, commercial_terms::CommercialTermsService},
};
use bss_pricing_sdk::{
    acceptance::{CommandMeta, NewSaleQuery},
    digest::selected_bindings_digest,
    read::{CatalogRef, PricingReadV1, ResolveQuery},
};
use std::sync::Arc;
use toolkit_security::SecurityContext;

pub struct FixedClock(pub parking_lot::Mutex<time::OffsetDateTime>);
impl FixedClock {
    pub fn advance(&self, by: time::Duration) {
        *self.0.lock() += by;
    }
}
impl Clock for FixedClock {
    fn now(&self) -> time::OffsetDateTime {
        *self.0.lock()
    }
}
pub struct AcceptanceFixture {
    pub denied_ctx: SecurityContext,
    pub fixture: p::Fixture,
    pub ctx: SecurityContext,
    pub query: NewSaleQuery,
    pub meta: CommandMeta,
    pub sellability: SellabilityProvider,
    pub acceptance: PricingAcceptanceProvider,
    pub read: PricingReadProvider,
    pub clock: Arc<FixedClock>,
    pub catalog: Arc<p::Catalog>,
}
impl AcceptanceFixture {
    pub fn fulfilment_query(
        receipt: &bss_pricing_sdk::acceptance::AcceptanceReceipt,
    ) -> bss_pricing_sdk::acceptance::FulfilmentQuery {
        bss_pricing_sdk::acceptance::FulfilmentQuery {
            tenant_axes: receipt.query.tenant_axes.clone(),
            acceptance: bss_pricing_sdk::acceptance::AcceptanceRef {
                acceptance_id: receipt.acceptance_id,
                terms_digest: receipt.terms_digest,
            },
            current_market: receipt.query.market.clone(),
            activation_at: receipt.query.start_at,
        }
    }
    pub async fn new() -> Self {
        let (fixture, catalog) = p::setup().await;
        Self::on(fixture, catalog).await
    }
    /// Build the same real service and catalog scenario on either migrated backend.
    pub async fn on(fixture: p::Fixture, catalog: Arc<p::Catalog>) -> Self {
        let book = p::book(&fixture, "acceptance").await;
        let sku = catalog.sku(bss_products_sdk::models::SkuType::Usage);
        let entry = p::policy_entry(&fixture, book, sku, "usage", None).await;
        s::put(
            &fixture,
            entry,
            s::Row {
                price: serde_json::json!({"rate":"10"}),
                ..s::Row::default()
            },
        )
        .await;
        let (plan, revision_id) = p::plan(&fixture, "acceptance", book).await;
        p::item(&fixture, revision_id, sku, Some(entry), "paid").await;
        p::publish(&fixture, p::id_of(&plan["id"]), revision_id).await;
        let mut content = catalog.content(sku);
        content.gl_code = Some("usage".into());
        content.tax_category = Some("standard".into());
        content.invoice_line_template = Some("{sku}".into());
        content.billing_timing = Some(bss_products_sdk::models::BillingTiming::Arrears);
        catalog.version(sku, 3, "2026-09-01", content);
        let ctx = fixture.ctx.clone();
        let enforcer = Arc::new(p::entry_support::enforcer_for(ctx.subject_tenant_id()));
        let read = PricingReadProvider::new(fixture.state.clone(), enforcer.clone());
        let mut query = s::sale_query();
        let resolved = read
            .resolve(
                &ctx,
                ResolveQuery {
                    catalog: CatalogRef {
                        tenant_id: ctx.subject_tenant_id(),
                    },
                    revision_id,
                    date: query.start_at.date(),
                    item_id: None,
                    pins: vec![],
                },
            )
            .await
            .unwrap();
        query.tenant_axes.seller_tenant_id = ctx.subject_tenant_id();
        query.plan_id = resolved.plan_id;
        query.plan_revision_id = resolved.revision_id;
        query.selections = resolved.cells.iter().map(|c| c.selection.clone()).collect();
        query.resolved_bindings_digest =
            selected_bindings_digest(&resolved, &query.selections).unwrap();
        let clock = Arc::new(FixedClock(parking_lot::Mutex::new(query.start_at)));
        let service = Arc::new(CommercialTermsService::new(
            fixture.state.clone(),
            enforcer,
            clock.clone(),
            SellerHoldPolicy::default(),
        ));
        Self {
            denied_ctx: p::holding(&fixture, "denied"),
            fixture,
            ctx,
            query,
            meta: CommandMeta {
                idempotency_key: "accept-1".into(),
            },
            sellability: SellabilityProvider::new(service.clone()),
            acceptance: PricingAcceptanceProvider::new(service),
            read,
            clock,
            catalog,
        }
    }
}

impl AcceptanceFixture {
    pub fn service(&self, policy: SellerHoldPolicy) -> Arc<CommercialTermsService> {
        Arc::new(CommercialTermsService::new(
            self.fixture.state.clone(),
            Arc::new(p::entry_support::enforcer_for(self.ctx.subject_tenant_id())),
            self.clock.clone(),
            policy,
        ))
    }
    pub async fn counts(&self) -> (i64, i64, i64) {
        use sea_orm::{ConnectionTrait, Database, Statement};
        let db = Database::connect(&self.fixture.dsn).await.unwrap();
        let row=db.query_one_raw(Statement::from_string(db.get_database_backend(),"SELECT (SELECT count(*) FROM pricing_acceptance) AS a, (SELECT count(*) FROM pricing_commercial_command) AS c, (SELECT count(*) FROM pricing_audit WHERE subject_kind='acceptance') AS audit")).await.unwrap().unwrap();
        (
            row.try_get("", "a").unwrap(),
            row.try_get("", "c").unwrap(),
            row.try_get("", "audit").unwrap(),
        )
    }
    pub async fn execute(&self, sql: &str) {
        use sea_orm::{ConnectionTrait, Database, Statement};
        let db = Database::connect(&self.fixture.dsn).await.unwrap();
        db.execute_raw(Statement::from_string(db.get_database_backend(), sql))
            .await
            .unwrap();
    }
    pub async fn resolved(&self) -> bss_pricing_sdk::read::ResolvedBindings {
        self.read
            .resolve(
                &self.ctx,
                ResolveQuery {
                    catalog: CatalogRef {
                        tenant_id: self.ctx.subject_tenant_id(),
                    },
                    revision_id: self.query.plan_revision_id,
                    date: self.query.start_at.date(),
                    item_id: None,
                    pins: vec![],
                },
            )
            .await
            .unwrap()
    }
    pub fn hook(
        &self,
        sql: Vec<String>,
        advance: Option<time::Duration>,
        repeat: bool,
    ) -> Arc<MeterHook> {
        let hook = Arc::new(MeterHook {
            dsn: self.fixture.dsn.to_string(),
            sql,
            once: std::sync::atomic::AtomicBool::new(false),
            repeat,
            clock: self.clock.clone(),
            advance,
            provider: p::entry_support::policy_support::MeterProvider::default(),
        });
        self.fixture
            .state
            .hub
            .register::<dyn bss_pricing_sdk::meter_semantics::UsageMeterSemanticsV1>(hook.clone());
        hook
    }
}
/// Interleave a committed catalog write while the meter is answering outside Pricing's tx.
pub struct MeterHook {
    dsn: String,
    sql: Vec<String>,
    once: std::sync::atomic::AtomicBool,
    repeat: bool,
    clock: Arc<FixedClock>,
    advance: Option<time::Duration>,
    pub provider: p::entry_support::policy_support::MeterProvider,
}
#[async_trait::async_trait]
impl bss_pricing_sdk::meter_semantics::UsageMeterSemanticsV1 for MeterHook {
    async fn resolve(
        &self,
        ctx: &SecurityContext,
        meter: bss_pricing_sdk::terms::MeterRef,
    ) -> Result<
        bss_pricing_sdk::meter_semantics::MeterSemantics,
        toolkit_canonical_errors::CanonicalError,
    > {
        if self.repeat || !self.once.swap(true, std::sync::atomic::Ordering::SeqCst) {
            use sea_orm::{ConnectionTrait, Database, Statement};
            let db = Database::connect(&self.dsn).await.unwrap();
            for sql in &self.sql {
                db.execute_raw(Statement::from_string(db.get_database_backend(), sql))
                    .await
                    .unwrap();
            }
            if let Some(by) = self.advance {
                self.clock.advance(by);
            }
        }
        self.provider.resolve(ctx, meter).await
    }
}

pub async fn schedule_replacement(f: &AcceptanceFixture) -> (uuid::Uuid, uuid::Uuid) {
    use bss_pricing::infra::storage::{
        entity::plan_revision,
        repo::{plan_revision_repo, price_book_entry_repo},
    };
    let tenant = f.ctx.subject_tenant_id();
    let scope = crate::plan_support::scope(&f.fixture);
    let conn = f.fixture.db.conn().unwrap();
    let old = plan_revision_repo::find(&conn, &scope, tenant, f.query.plan_revision_id)
        .await
        .unwrap()
        .unwrap();
    let binding = f.resolved().await.cells[0].binding.clone().unwrap();
    let entry = price_book_entry_repo::find(&conn, &scope, tenant, binding.price_book_entry_id)
        .await
        .unwrap()
        .unwrap();
    let policy = crate::plan_support::entry_support::policy_support::input();
    let mut content: bss_pricing::infra::usage_policy_wire::UsageRatingPolicyInput =
        serde_json::from_value(policy).unwrap();
    content.rating_window = bss_pricing::infra::usage_policy_wire::RatingWindow::CalendarHour {
        timezone: bss_pricing::infra::usage_policy_wire::Timezone::Utc,
    };
    let policy = bss_pricing::infra::storage::repo::usage_policy_repo::intern(
        &conn,
        &scope,
        tenant,
        f.ctx.subject_id(),
        &content,
        f.query.start_at,
    )
    .await
    .unwrap();
    let new_entry = crate::plan_support::entry_with_policy(
        &f.fixture,
        entry.book_id,
        entry.sku_id,
        "usage",
        None,
        "per_unit",
        Some(policy),
    )
    .await;
    crate::seam_support::put(
        &f.fixture,
        new_entry,
        crate::seam_support::Row {
            price: serde_json::json!({"rate":"12"}),
            ..Default::default()
        },
    )
    .await;
    let id = uuid::Uuid::new_v4();
    plan_revision_repo::insert(
        &conn,
        &scope,
        plan_revision::Model {
            id,
            rev_no: 2,
            state: "draft".into(),
            available_from: Some(f.query.start_at.date() + time::Duration::days(1)),
            pending_unit_id: None,
            approved_by_unit_id: None,
            published_at: None,
            version: 1,
            ..old
        },
    )
    .await
    .unwrap();
    crate::plan_support::item(&f.fixture, id, entry.sku_id, Some(new_entry), "paid").await;
    let unit = crate::plan_support::lock(&f.fixture, id).await;
    plan_revision_repo::schedule(&conn, &scope, tenant, id, unit, f.query.start_at)
        .await
        .unwrap();
    (id, new_entry)
}
