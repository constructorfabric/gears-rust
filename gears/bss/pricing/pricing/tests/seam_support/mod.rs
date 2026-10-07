//! Shared stored read fixtures for REST and SDK conformance.
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(dead_code)]
use crate::plan_support;
use crate::plan_support::{Catalog, Fixture, book, id_of, item, plan, publish, scope, setup};
use bss_pricing::infra::storage::{
    entity::{price, price_book_entry},
    repo::{price_book_entry_repo, price_repo},
};
use bss_products_sdk::models::{BillingTiming, SkuType};
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;
pub fn date(text: &str) -> time::Date {
    time::Date::parse(text, &time::format_description::well_known::Iso8601::DATE).unwrap()
}
/// One stored price row; the defaults are an approved open flat price of the default chain. Its
/// model is its entry's (D-427): `entry_of` gives a recurring entry `flat`, a usage one `per_unit`.
#[derive(Clone)]
pub struct Row {
    pub dim: Option<&'static str>,
    pub price: Value,
    pub min_fee: Option<&'static str>,
    pub from: &'static str,
    pub to: Option<&'static str>,
    pub eligibility: &'static str,
    pub state: &'static str,
    pub keep: bool,
    pub closed: bool,
    pub temporary_until: Option<&'static str>,
    pub version_no: i32,
    /// `set` for a price; `cancel` or `end` for a change row naming `target` (D-520, D-521).
    pub change_kind: &'static str,
    pub target: Option<Uuid>,
}
impl Default for Row {
    fn default() -> Self {
        Self {
            dim: None,
            price: json!({"amount":"30.00"}),
            min_fee: None,
            from: "2026-09-01",
            to: None,
            eligibility: "all",
            state: "approved",
            keep: false,
            closed: false,
            temporary_until: None,
            version_no: 1,
            change_kind: "set",
            target: None,
        }
    }
}
pub fn flat(amount: &str) -> Value {
    json!({ "amount": amount })
}
/// Write one price of `entry` straight through the repository.
pub async fn put(f: &Fixture, entry: Uuid, row: Row) -> Uuid {
    let now = time::OffsetDateTime::now_utc();
    let pending_unit_id = if row.state == "pending" {
        Some(plan_support::unit_of_kind(f, "prices").await)
    } else {
        None
    };
    // A cancelled price names the unit that cancelled it (the pairing CHECK of 000022).
    let cancelled_by_unit_id = if row.state == "cancelled" {
        Some(plan_support::unit_of_kind(f, "prices").await)
    } else {
        None
    };
    price_repo::insert(
        &f.db.conn().unwrap(),
        &scope(f),
        price::Model {
            id: Uuid::now_v7(),
            tenant_id: f.ctx.subject_tenant_id(),
            price_book_entry_id: entry,
            version_no: row.version_no,
            dim_value: row.dim.map(str::to_owned),
            price_json: row.price,
            min_fee: row.min_fee.map(str::to_owned),
            eligibility: row.eligibility.into(),
            effective_from: date(row.from),
            effective_to: row.to.map(date),
            keep_for_bound: row.keep,
            closed_explicitly: row.closed,
            temporary_until: row.temporary_until.map(date),
            paired_price_id: None,
            return_of_price_id: None,
            change_kind: row.change_kind.into(),
            target_price_id: row.target,
            cancelled_by_unit_id,
            state: row.state.into(),
            pending_unit_id,
            approved_by_unit_id: None,
            note: Some("authoring note".into()),
            created_by: f.ctx.subject_id(),
            approved_at: None,
            version: 3,
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap()
    .id
}
/// An entry of `book` for `sku`, written directly, with a dimension key and an invoice-line
/// override when asked.
pub async fn entry_of(
    f: &Fixture,
    book: Uuid,
    sku: Uuid,
    kind: &str,
    shape: (Option<&str>, Option<&str>, Option<&str>),
) -> Uuid {
    let (period, key, line) = shape;
    let now = time::OffsetDateTime::now_utc();
    price_book_entry_repo::insert(
        &f.db.conn().unwrap(),
        &scope(f),
        price_book_entry::Model {
            id: Uuid::now_v7(),
            tenant_id: f.ctx.subject_tenant_id(),
            book_id: book,
            sku_id: sku,
            charge_kind: kind.into(),
            period: period.map(str::to_owned),
            model: bss_pricing::domain::price_book_entry::default_model(kind.parse().unwrap())
                .as_str()
                .into(),
            usage_policy_id: None,
            usage_policy_version: None,
            usage_policy_digest: None,
            usage_sku_version: None,
            dimension_key: key.map(str::to_owned),
            invoice_line_override: line.map(str::to_owned),
            reservation_id: Uuid::new_v4(),
            reference_state: "confirmed".into(),
            version: 1,
            created_at: now,
            updated_at: now,
        },
    )
    .await
    .unwrap()
    .id
}
/// Register one dimension key through its door.
pub async fn dimension(f: &Fixture, key: &str, values: &[&str]) {
    let (_, _, tag) = f
        .call("GET", "/dimension-keys", json!({}), None, None)
        .await;
    let (s, b, _) = f
        .call(
            "PUT",
            "/dimension-keys",
            json!({"items":[{"key":key,"values":values}]}),
            Some(&tag),
            None,
        )
        .await;
    assert_eq!(s, 200, "{b}");
}
/// Write the tenant settings through their door.
pub async fn settings(f: &Fixture, body: Value) {
    let (_, _, tag) = f.call("GET", "/settings", json!({}), None, None).await;
    let (s, b, _) = f.call("PUT", "/settings", body, Some(&tag), None).await;
    assert_eq!(s, 200, "{b}");
}
pub async fn resolve(f: &Fixture, query: &str) -> (u16, Value) {
    let (s, b, tag) = f
        .call("GET", &format!("/resolve?{query}"), json!({}), None, None)
        .await;
    assert_eq!(tag, "", "a resolve answer carries no ETag");
    (s, b)
}
pub async fn resolve_as(
    f: &Fixture,
    ctx: &toolkit_security::SecurityContext,
    query: &str,
) -> (u16, Value) {
    let (s, b, _) = f
        .call_as(
            ctx,
            "GET",
            &format!("/resolve?{query}"),
            json!({}),
            None,
            None,
        )
        .await;
    (s, b)
}
/// Rows of the tables a mutation writes: audit, idempotency, outbox and reference ops.
pub async fn written(f: &Fixture) -> Vec<i64> {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    let db = Database::connect(&f.dsn).await.unwrap();
    let mut counts = Vec::new();
    for table in [
        "pricing_audit",
        "pricing_idempotency",
        "bss_pricing_outbox_body",
        "pricing_reference_op",
    ] {
        let row = db
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                format!("SELECT COUNT(*) AS n FROM {table}"),
            ))
            .await
            .unwrap()
            .unwrap();
        counts.push(row.try_get::<i64>("", "n").unwrap());
    }
    counts
}

/// A published plan: one monthly recurring SKU priced €30 from 2026-09-01, as one paid item.
pub struct World {
    pub f: Fixture,
    pub catalog: Arc<Catalog>,
    pub book: Uuid,
    pub plan: Uuid,
    pub revision: Uuid,
    pub sku: Uuid,
    pub entry: Uuid,
    pub item: Uuid,
    pub price: Uuid,
}
pub async fn world() -> World {
    let (f, catalog) = setup().await;
    let book = book(&f, "eur").await;
    let sku = catalog.sku(SkuType::Recurring);
    let entry = entry_of(&f, book, sku, "recurring", (Some("month"), None, None)).await;
    let price = put(&f, entry, Row::default()).await;
    let (created, revision) = plan(&f, "pro", book).await;
    let plan = id_of(&created["id"]);
    let item = item(&f, revision, sku, Some(entry), "paid").await.id;
    publish(&f, plan, revision).await;
    World {
        f,
        catalog,
        book,
        plan,
        revision,
        sku,
        entry,
        item,
        price,
    }
}

use bss_pricing::api::pricing_read::PricingReadProvider;
use bss_pricing_sdk::read::{CatalogRef, PriceQuery, ResolveQuery};
use toolkit_security::SecurityContext;

pub struct ReadFixture {
    pub provider: PricingReadProvider,
    pub ctx: SecurityContext,
    pub denied_ctx: SecurityContext,
    pub price_query: PriceQuery,
    pub resolve_query: ResolveQuery,
    pub fixture: Fixture,
}
impl ReadFixture {
    pub async fn new() -> Self {
        let (fixture, catalog) = setup().await;
        let book = book(&fixture, "eur").await;
        let sku = catalog.sku(SkuType::Usage);
        let entry = entry_of(&fixture, book, sku, "usage", (None, None, None)).await;
        let price_id = put(
            &fixture,
            entry,
            Row {
                price: json!({"rate":"0.047"}),
                ..Row::default()
            },
        )
        .await;
        let (created, revision_id) = plan(&fixture, "seam", book).await;
        item(&fixture, revision_id, sku, Some(entry), "paid").await;
        publish(&fixture, id_of(&created["id"]), revision_id).await;
        let mut content = catalog.content(sku);
        content.unit = Some("cloudlet_hour".into());
        content.gl_code = Some("usage".into());
        content.tax_category = Some("standard".into());
        content.invoice_line_template = Some("{sku}".into());
        content.billing_timing = Some(BillingTiming::Arrears);
        catalog.version(sku, 1, "2026-09-01", content);
        let ctx = fixture.ctx.clone();
        let catalog = CatalogRef {
            tenant_id: ctx.subject_tenant_id(),
        };
        let provider = PricingReadProvider::new(
            fixture.state.clone(),
            Arc::new(plan_support::entry_support::enforcer_for(catalog.tenant_id)),
        );
        Self {
            provider,
            ctx,
            denied_ctx: plan_support::holding(&fixture, "denied"),
            price_query: PriceQuery {
                catalog: catalog.clone(),
                price_id,
            },
            resolve_query: ResolveQuery {
                catalog,
                revision_id,
                date: date("2026-09-15"),
                item_id: None,
                pins: vec![],
            },
            fixture,
        }
    }
}

/// Stable policy fixture; content identity is independently covered by SDK digest vectors.
pub fn vm_hour_policy() -> bss_pricing_sdk::terms::UsageRatingPolicy {
    use bss_pricing_sdk::terms::*;
    let content = UsageRatingPolicyInput {
        rating_window: RatingWindow::BillingCycle,
        aggregation_scope: AggregationScope::SubscriptionLine,
        reset: Reset::RatingWindowStart,
        partial_window: PartialWindow::ActualQuantityFullThresholds,
        fold: Fold::Sum,
    };
    UsageRatingPolicy {
        policy_id: Uuid::from_u128(1),
        version: 1,
        digest: bss_pricing_sdk::digest::policy_digest(&content),
        content,
    }
}

/// Complete deterministic invoice and usage binding, independent of providers and storage.
pub fn vm_binding() -> bss_pricing_sdk::read::AcceptedBinding {
    use bss_pricing_sdk::{digest::*, read::*, terms::*};
    let mut price = ImmutablePrice {
        price_id: Uuid::from_u128(3),
        price_book_entry_id: Uuid::from_u128(2),
        money_digest: [0; 32],
        currency: "EUR".into(),
        model: PriceModel::PerUnit {
            unit_amount: "0.047".parse().unwrap(),
        },
        minimum_fee: None,
        effective_from: date("2026-10-01"),
        ends_on: None,
        state: PriceState::Approved,
    };
    price.money_digest = money_digest(&price);
    AcceptedBinding {
        item_id: Uuid::from_u128(4),
        price_book_entry_id: Uuid::from_u128(2),
        dimension_key: None,
        dimension_value: None,
        sku_id: Uuid::from_u128(5),
        sku_version: 3,
        sku_code: "VM-2CPU-4GB".into(),
        sku_name: "VM 2 vCPU / 4 GB".into(),
        unit: Some("VM\u{b7}hour".into()),
        meter: Some(MeterRef {
            usage_type_id: "vm-hours".into(),
            version: "v1".into(),
        }),
        price,
        kind: ChargeKind::Usage,
        recurring_period: None,
        via_default: false,
        usage_rating_policy: Some(vm_hour_policy()),
        invoice: InvoiceInputs {
            template: "VM usage".into(),
            template_digest: template_digest("VM usage"),
            template_source: InputSource::SkuVersion,
            gl_code: "VM_REVENUE".into(),
            tax_category: "cloud-services".into(),
            timing: bss_pricing_sdk::terms::BillingTiming::Arrears,
            currency_scale: 2,
            rounding: Rounding::HalfEven,
        },
    }
}
/// Explicit monthly invoice terms with a valid calendar anchor and exactly one selection.
pub fn sale_query() -> bss_pricing_sdk::acceptance::NewSaleQuery {
    use bss_pricing_sdk::{acceptance::*, digest::*, read::*, terms::*};
    let at = date("2026-10-01").midnight().assume_utc();
    let mut billing_terms = BillingTerms {
        schema_version: 1,
        cycle: BillingCycle::Month,
        anchor: BillingAnchor::Calendar,
        anchor_at: at,
        timezone: Timezone::Utc,
        source: TermsSource::ExplicitOrder,
        digest: [0; 32],
    };
    billing_terms.digest = billing_terms_digest(&billing_terms);
    let binding = vm_binding();
    let selections = vec![BindingSelection {
        item_id: binding.item_id,
        dimension_value: None,
    }];
    let resolved = ResolvedBindings {
        plan_id: Uuid::from_u128(6),
        revision_id: Uuid::from_u128(7),
        cells: vec![ResolvedCell {
            selection: selections[0].clone(),
            binding: Some(binding),
        }],
    };
    NewSaleQuery {
        tenant_axes: TenantAxes {
            seller_tenant_id: Uuid::from_u128(8),
            payer_tenant_id: Uuid::from_u128(9),
            resource_tenant_id: Uuid::from_u128(10),
        },
        order_id: Uuid::from_u128(11),
        order_version: 1,
        line_id: Uuid::from_u128(12),
        plan_id: resolved.plan_id,
        plan_revision_id: resolved.revision_id,
        resolved_bindings_digest: selected_bindings_digest(&resolved, &selections).unwrap(),
        selections,
        quantity: rust_decimal::Decimal::ONE,
        market: Market {
            currency: "EUR".into(),
            region: None,
        },
        start_at: at,
        term: Term::Rolling,
        billing_terms,
        hold_policy_version: 1,
    }
}

/// Compare complete commercial pins, matching by item rather than incidental storage order.
pub fn compare_bindings(
    actual: &[bss_pricing_sdk::read::AcceptedBinding],
    expected: &[bss_pricing_sdk::read::AcceptedBinding],
) {
    assert_eq!(actual.len(), expected.len());
    for e in expected {
        let a = actual.iter().find(|b| b.item_id == e.item_id).unwrap();
        assert_eq!(a, e, "all entry/policy/money/descriptor pins must survive");
    }
}

/// Durable rows observed after two synchronized identical commands.
pub struct RaceResult {
    pub receipt_ids: Vec<Uuid>,
    pub stored_acceptances: usize,
    pub stored_commands: usize,
}

/// Real acceptance race on a caller-supplied migrated database.
pub async fn run_acceptance_race(db: toolkit_db::DBProvider<toolkit_db::DbError>) -> RaceResult {
    use bss_pricing::infra::storage::repo::{acceptance_repo, commercial_command_repo};
    use bss_pricing_sdk::acceptance::SellabilityV1;
    // This scenario uses repository reads only; no raw connection needs the fixture DSN.
    let catalog = Arc::new(plan_support::Catalog::default());
    let fixture = plan_support::Fixture::on(
        db,
        Uuid::new_v4(),
        plan_support::entry_support::TestDsn::of(String::new()),
        catalog.clone(),
    )
    .await;
    let f = acceptance_fixture::AcceptanceFixture::on(fixture, catalog).await;
    let barrier = tokio::sync::Barrier::new(2);
    let call = || async {
        barrier.wait().await;
        f.sellability
            .check(&f.ctx, f.query.clone(), f.meta.clone())
            .await
            .unwrap()
    };
    let (a, b) = tokio::join!(call(), call());
    let conn = f.fixture.db.conn().unwrap();
    let scope = scope(&f.fixture);
    let tenant = f.ctx.subject_tenant_id();
    let stored = acceptance_repo::find_business(
        &conn,
        &scope,
        tenant,
        f.query.order_id,
        &f.query.order_version.to_string(),
        f.query.line_id,
    )
    .await
    .unwrap()
    .unwrap();
    let command = commercial_command_repo::find_scope(
        &conn,
        &scope,
        &commercial_command_repo::CommandScope {
            tenant_id: tenant,
            caller_tenant_id: tenant,
            caller_id: f.ctx.subject_id(),
            operation: "check".into(),
            idempotency_key: f.meta.idempotency_key.clone(),
        },
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(stored.id, a.acceptance_id);
    assert_eq!(command.receipt_id, stored.id);
    let (stored_acceptances, stored_commands) = commercial_counts(&f.fixture).await;
    RaceResult {
        receipt_ids: vec![a.acceptance_id, b.acceptance_id],
        stored_acceptances,
        stored_commands,
    }
}

/// Tenant-scoped persisted rows, independent of provider/mock call counts.
pub async fn commercial_counts(f: &Fixture) -> (usize, usize) {
    use bss_pricing::infra::storage::entity::{acceptance, commercial_command};
    use sea_orm::EntityTrait;
    use toolkit_db::secure::SecureEntityExt;
    let conn = f.db.conn().unwrap();
    let scope = scope(f);
    let a = acceptance::Entity::find()
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .unwrap();
    let c = commercial_command::Entity::find()
        .secure()
        .scope_with(&scope)
        .all(&conn)
        .await
        .unwrap();
    (a.len(), c.len())
}

pub mod acceptance_fixture;
