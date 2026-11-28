//! A plan revision with quota rules, sold through the real acceptance service (D-547, D-548).
//!
//! The plan is the acceptance fixture's: one usage item of a SKU at version 3 from 2026-09-01,
//! priced at 10 per unit from 2026-09-01. Its rules are saved through the `PUT` while it is a
//! draft, then it is published as an applied unit publishes it. Products' quota catalog double
//! holds enabled vCPU and RAM definitions of the item's SKU. A sale is built as a buyer builds it
//! from the preview: the resolve answer, its selected bindings digest and the selected quota
//! digest of the requested cells.
#![allow(dead_code, clippy::expect_used, clippy::unwrap_used)]
use crate::seam_support::acceptance_fixture::FixedClock;
use crate::{plan_support as p, quota_support as q, seam_support as s};
use bss_pricing::{
    api::{
        pricing_acceptance::PricingAcceptanceProvider, pricing_read::PricingReadProvider,
        sellability::SellabilityProvider,
    },
    config::SellerHoldPolicy,
    infra::commercial_terms::CommercialTermsService,
};
use bss_pricing_sdk::{
    acceptance::{AcceptanceReceipt, CommandMeta, NewSaleQuery, SellabilityV1},
    digest::{selected_bindings_digest, selected_quota_digest},
    read::{BindingSelection, CatalogRef, PricingReadV1, ResolveQuery, ResolvedBindings},
};
use bss_products_sdk::models::{BillingTiming, SkuType};
use bss_products_sdk::quota::catalog::QuotaDefinition;
use bss_products_sdk::quota::validation::{QuotaSetRequest, QuotaValidationPurpose};
use serde_json::{Value, json};
use std::sync::Arc;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// The metric a rule's definition measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    Vcpu,
    Ram,
}

/// One rule of a sale's plan: its id `Uuid::from_u128(n)`, its definition's metric, the item it
/// sets (`included` for the item without an entry, whose rule is always vCPU), its dimension value
/// (`None`: the item's default rule), its binding, amount and basis. Every rule keeps the scope
/// the fixture's policy asks for, so the rules of one binding share one scope.
#[derive(Debug, Clone, Copy)]
pub struct Rule {
    pub n: u128,
    pub metric: Metric,
    pub included: bool,
    pub value: Option<&'static str>,
    pub binding: &'static str,
    pub amount: &'static str,
    pub basis: &'static str,
}

/// Rule `n` of the priced item for `value`, on the binding `compute`, per plan unit.
#[must_use]
pub const fn rule(
    n: u128,
    metric: Metric,
    value: Option<&'static str>,
    amount: &'static str,
) -> Rule {
    Rule {
        n,
        metric,
        included: false,
        value,
        binding: "compute",
        amount,
        basis: "per_plan_unit",
    }
}

/// How the sale's plan is built.
#[derive(Debug, Clone, Copy)]
pub struct Shape {
    /// The values of `region` the priced item's entry is keyed by, each value's chain priced from
    /// 2026-09-01; empty for an entry without a dimension.
    pub regions: &'static [&'static str],
    /// The first day of the price on the entry's default chain; `None` for no such price.
    pub default_from: Option<&'static str>,
    /// A second item, included and without an entry, so no sale can select it: a legacy item
    /// without an entry, published past the checks. Submit refuses a revision with an item
    /// without an entry (`ITEM_ENTRY_MISSING`, D-467; D-512 keeps that refusal for an item added
    /// without one), so only this fixture's direct publication gives a published revision a rule
    /// on such an item.
    pub included_item: bool,
    /// The definitions are disabled while their qualification is pending.
    pub disabled: bool,
    /// The revision's sale date, written before the rules are saved; `None` for "at publish".
    pub available_from: Option<&'static str>,
}

impl Default for Shape {
    fn default() -> Self {
        Self {
            regions: &[],
            default_from: Some("2026-09-01"),
            included_item: false,
            disabled: false,
            available_from: None,
        }
    }
}

/// A published plan with quota rules and the real providers that sell it.
pub struct QuotaSale {
    pub f: p::Fixture,
    pub catalog: Arc<p::Catalog>,
    pub fake: Arc<q::FakeQuotaCatalog>,
    pub ctx: SecurityContext,
    pub read: PricingReadProvider,
    pub sellability: SellabilityProvider,
    pub acceptance: PricingAcceptanceProvider,
    pub clock: Arc<FixedClock>,
    pub plan: Uuid,
    pub revision: Uuid,
    /// The priced item.
    pub item: Uuid,
    /// The priced item's entry.
    pub entry: Uuid,
    /// The priced item's SKU.
    pub sku: Uuid,
    /// The included item without an entry, when the shape asks for it.
    pub included: Option<Uuid>,
    /// The vCPU and RAM definitions of the priced item's SKU.
    pub cpu: QuotaDefinition,
    pub ram: QuotaDefinition,
    /// The vCPU definition of the included item's SKU.
    pub included_cpu: Option<QuotaDefinition>,
    /// The saved rule set, as the `PUT` answered it; `Null` for a plan without rules.
    pub saved: Value,
    /// The draft's checks just before publication; `Null` for a plan without rules.
    pub draft_checks: Value,
}

/// A plan of the default shape with `rules`.
pub async fn sale_with_rules(rules: &[Rule]) -> QuotaSale {
    sale_shaped(Shape::default(), rules).await
}

/// A plan of `shape` with `rules`, published. The build runs boxed, so a test's future stays
/// small.
pub async fn sale_shaped(shape: Shape, rules: &[Rule]) -> QuotaSale {
    Box::pin(build(shape, rules)).await
}

/// Price the entry as `shape` asks: each region's chain from 2026-09-01, then the default chain
/// from `default_from`, each at 10 per unit.
async fn price_entry(f: &p::Fixture, entry: Uuid, shape: Shape) {
    let chains = shape
        .regions
        .iter()
        .map(|value| (Some(*value), "2026-09-01"))
        .chain(shape.default_from.map(|from| (None, from)));
    for (version_no, (dim, from)) in (1..).zip(chains) {
        let row = s::Row {
            dim,
            price: json!({"rate": "10"}),
            from,
            version_no,
            ..s::Row::default()
        };
        Box::pin(s::put(f, entry, row)).await;
    }
}

/// Each rule as the `PUT` body carries it: its definition by metric on the priced item, or the
/// included item's vCPU definition.
fn rule_bodies(
    rules: &[Rule],
    priced: (Uuid, &QuotaDefinition, &QuotaDefinition),
    included: Option<(Uuid, &QuotaDefinition)>,
) -> Vec<Value> {
    let (item, cpu, ram) = priced;
    rules
        .iter()
        .map(|r| {
            let (definition, on) = match (r.included, r.metric) {
                (true, _) => {
                    let (id, definition) = included.unwrap();
                    (definition, id)
                }
                (false, Metric::Vcpu) => (cpu, item),
                (false, Metric::Ram) => (ram, item),
            };
            let mut rule = q::rule_json(r.n, definition, on, r.amount);
            rule["dimension_value"] = json!(r.value);
            rule["logical_binding_key"] = json!(r.binding);
            rule["basis"] = json!(r.basis);
            rule
        })
        .collect()
}

/// Save `body` on draft `revision` and read its checks: the saved set and the checks answer.
async fn save_and_check(f: &p::Fixture, revision: Uuid, body: &[Value]) -> (Value, Value) {
    let (status, saved) = Box::pin(put_rules(f, revision, body, "quota-sale-rules")).await;
    assert_eq!(status, 200, "{saved}");
    let path = format!("/plan-revisions/{revision}/checks");
    let (status, checks, _) = Box::pin(f.call("GET", &path, json!(null), None, None)).await;
    assert_eq!(status, 200, "{checks}");
    (saved, checks)
}

async fn build(shape: Shape, rules: &[Rule]) -> QuotaSale {
    let (f, catalog) = Box::pin(p::setup()).await;
    let fake = q::install(&f);
    let book = Box::pin(p::book_in(&f, "quota-sale", "EUR")).await;
    let sku = catalog.sku(SkuType::Usage);
    let entry = Box::pin(p::policy_entry(&f, book, sku, "usage", None)).await;
    if !shape.regions.is_empty() {
        Box::pin(q::priced_by_region_values(&f, shape.regions)).await;
    }
    price_entry(&f, entry, shape).await;
    let (plan, revision) = Box::pin(p::plan(&f, "quotasale", book)).await;
    let plan = p::id_of(&plan["id"]);
    let item = Box::pin(p::item(&f, revision, sku, Some(entry), "paid"))
        .await
        .id;
    let mut content = catalog.content(sku);
    content.gl_code = Some("usage".into());
    content.tax_category = Some("standard".into());
    content.invoice_line_template = Some("{sku}".into());
    content.billing_timing = Some(BillingTiming::Arrears);
    catalog.version(sku, 3, "2026-09-01", content);
    let enabled = |definition: QuotaDefinition| {
        if shape.disabled {
            q::disabled(definition)
        } else {
            definition
        }
    };
    let cpu = fake.add(enabled(q::vcpu(sku)));
    let ram = fake.add(enabled(q::ram(sku)));
    let included = if shape.included_item {
        let other = catalog.sku(SkuType::Usage);
        catalog.version(other, 3, "2026-09-01", catalog.content(other));
        let id = Box::pin(p::item(&f, revision, other, None, "included"))
            .await
            .id;
        Some((id, fake.add(enabled(q::vcpu(other)))))
    } else {
        None
    };
    if let Some(from) = shape.available_from {
        available_from(&f, revision, from).await;
    }
    let (saved, draft_checks) = if rules.is_empty() {
        (Value::Null, Value::Null)
    } else {
        let included_ref = included.as_ref().map(|(id, definition)| (*id, definition));
        let body = rule_bodies(rules, (item, &cpu, &ram), included_ref);
        save_and_check(&f, revision, &body).await
    };
    Box::pin(p::publish(&f, plan, revision)).await;
    let ctx = f.ctx.clone();
    let enforcer = Arc::new(p::entry_support::enforcer_for(ctx.subject_tenant_id()));
    let read = PricingReadProvider::new(f.state.clone(), enforcer.clone());
    let clock = Arc::new(FixedClock(parking_lot::Mutex::new(
        s::sale_query().start_at,
    )));
    let service = Arc::new(CommercialTermsService::new(
        f.state.clone(),
        enforcer,
        clock.clone(),
        SellerHoldPolicy::default(),
    ));
    QuotaSale {
        f,
        catalog,
        fake,
        ctx,
        read,
        sellability: SellabilityProvider::new(service.clone()),
        acceptance: PricingAcceptanceProvider::new(service),
        clock,
        plan,
        revision,
        item,
        entry,
        sku,
        included: included.as_ref().map(|(id, _)| *id),
        cpu,
        ram,
        included_cpu: included.map(|(_, definition)| definition),
        saved,
        draft_checks,
    }
}

/// Write `from` as the revision's sale date, as stored.
pub async fn available_from(f: &p::Fixture, revision: Uuid, from: &str) {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    let changed = Database::connect(&f.dsn)
        .await
        .unwrap()
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "UPDATE pricing_plan_revision SET available_from = ? WHERE id = ?",
            [from.into(), revision.into()],
        ))
        .await
        .unwrap();
    assert_eq!(changed.rows_affected(), 1);
}

/// `PUT` the whole set `rules` on draft `revision` under its current `ETag` and the key `key`.
pub async fn put_rules(f: &p::Fixture, revision: Uuid, rules: &[Value], key: &str) -> (u16, Value) {
    let (s, read, etag) = f
        .call(
            "GET",
            &format!("/plan-revisions/{revision}"),
            json!(null),
            None,
            None,
        )
        .await;
    assert_eq!(s, 200, "{read}");
    let (s, body, _) = f
        .call(
            "PUT",
            &format!("/plan-revisions/{revision}/quota-rules"),
            json!({ "quota_rules": rules }),
            Some(&etag),
            Some(key),
        )
        .await;
    (s, body)
}

impl QuotaSale {
    /// `revision` resolved on the sale's start date through the typed read.
    pub async fn resolved_of(&self, revision: Uuid) -> ResolvedBindings {
        self.read
            .resolve(
                &self.ctx,
                ResolveQuery {
                    catalog: CatalogRef {
                        tenant_id: self.ctx.subject_tenant_id(),
                    },
                    revision_id: revision,
                    date: s::sale_query().start_at.date(),
                    item_id: None,
                    pins: vec![],
                },
            )
            .await
            .unwrap()
    }

    /// The sale's revision resolved on its start date.
    pub async fn resolved(&self) -> ResolvedBindings {
        self.resolved_of(self.revision).await
    }

    /// A new sale of the priced item's `value` cell (and the market's region `value`) at
    /// `quantity` plan units, on a new order line, with the preview's selected bindings digest and,
    /// for a revision with rules, its selected quota digest.
    pub async fn sale(&self, value: Option<&str>, quantity: &str) -> NewSaleQuery {
        let resolved = self.resolved().await;
        sale_of(&self.ctx, &resolved, self.item, value, quantity)
    }

    /// `sale`'s key.
    #[must_use]
    pub fn meta(key: &str) -> CommandMeta {
        CommandMeta {
            idempotency_key: key.into(),
        }
    }

    /// Accept `query` under `key`.
    /// # Errors
    /// The acceptance's refusal.
    pub async fn check(
        &self,
        query: &NewSaleQuery,
        key: &str,
    ) -> Result<AcceptanceReceipt, CanonicalError> {
        self.sellability
            .check(&self.ctx, query.clone(), Self::meta(key))
            .await
    }

    /// The requests Products was asked to judge for an acceptance.
    pub fn acceptance_requests(&self) -> Vec<QuotaSetRequest> {
        self.fake
            .requests()
            .into_iter()
            .filter(|r| r.purpose == QuotaValidationPurpose::Acceptance)
            .collect()
    }

    /// The stored acceptances.
    pub async fn acceptances(&self) -> i64 {
        use sea_orm::{ConnectionTrait, Database, Statement};
        let db = Database::connect(&self.f.dsn).await.unwrap();
        db.query_one_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT count(*) AS n FROM pricing_acceptance",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "n")
        .unwrap()
    }
}

/// A new sale of `item`'s `value` cell of `resolved` at `quantity`, as a buyer builds it from the
/// preview: the selected bindings digest of the selection and, for a revision with rules, its
/// selected quota digest; a new order line each time.
pub fn sale_of(
    ctx: &SecurityContext,
    resolved: &ResolvedBindings,
    item: Uuid,
    value: Option<&str>,
    quantity: &str,
) -> NewSaleQuery {
    let mut query = s::sale_query();
    query.tenant_axes.seller_tenant_id = ctx.subject_tenant_id();
    query.line_id = Uuid::new_v4();
    query.plan_id = resolved.plan_id;
    query.plan_revision_id = resolved.revision_id;
    query.selections = vec![BindingSelection {
        item_id: item,
        dimension_value: value.map(str::to_owned),
    }];
    query.market.region = value.map(str::to_owned);
    query.quantity = quantity.parse().unwrap();
    query.resolved_bindings_digest = selected_bindings_digest(resolved, &query.selections).unwrap();
    query.expected_quota_digest = resolved
        .quota_terms
        .as_ref()
        .map(|terms| selected_quota_digest(terms, &query.selections));
    query
}

/// The concrete reason of an acceptance refusal.
#[must_use]
pub fn reason(error: &CanonicalError) -> String {
    bss_pricing::infra::commercial_terms::errors::commercial_reason(error).unwrap_or_default()
}
