//! Executable Pricing portions of atlas F02/F07/F22/F23/F24/F31.
//! Hour scheduling, source integration and invoice roll-ups are external obligations.
#![allow(clippy::expect_used, clippy::unwrap_used)]
mod plan_support;
mod seam_support;
use bss_pricing::infra::usage_policy_wire as wire;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Scenario {
    VmHour,
    CloudletsHourlyVolume,
    CloudletsHourlyGraduated,
    FrozenAcceptance,
    UnsupportedTerms,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    schema_version: u32,
    atlas_fixture_ids: Vec<String>,
    scenario: Scenario,
    given: Given,
    expected: Expected,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Given {
    commercial: Commercial,
    descriptors: Descriptors,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    successor_rate: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Expected {
    commercial: Commercial,
    descriptors: Descriptors,
    arithmetic: Vec<Amount>,
    refusals: Vec<Refusal>,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Amount {
    quantity: String,
    amount: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Refusal {
    MissingPolicy,
    MinimumFee,
    UnalignedAnchor,
    Package,
    MissingBillingTerms,
    Quarterly,
    CrossLinePooling,
    IncludedQuantity,
    Phases,
    Promotions,
    Fx,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Descriptors {
    sku_version: i64,
    code: String,
    name: String,
    gl_code: String,
    tax_category: String,
    template: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Model {
    PerUnit,
    Volume,
    Graduated,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Tier {
    up_to: Option<String>,
    rate: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Quantity {
    usage_type_id: String,
    usage_type_version: String,
    unit: String,
    fold: wire::Fold,
    accrual_policy_version: String,
}
/// Combined consumer view; policy belongs to the entry, money to its price, cycle to the sale.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Commercial {
    billing_cycle: String,
    entry_period: Option<String>,
    rating_window: wire::RatingWindow,
    aggregation_scope: wire::AggregationScope,
    reset: wire::Reset,
    quantity_semantics: Quantity,
    partial_window: wire::PartialWindow,
    model: Model,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rate: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tiers: Vec<Tier>,
    minimum_fee: Option<String>,
}
fn fixtures() -> Vec<Fixture> {
    [
        include_str!("seam_fixtures/vm-hour.json"),
        include_str!("seam_fixtures/cloudlets-hourly-volume.json"),
        include_str!("seam_fixtures/cloudlets-hourly-graduated.json"),
        include_str!("seam_fixtures/frozen-acceptance.json"),
        include_str!("seam_fixtures/unsupported-terms.json"),
    ]
    .into_iter()
    .map(|text| serde_json::from_str(text).unwrap())
    .collect()
}
#[test]
fn all_five_fixture_envelopes_round_trip_through_closed_typed_dtos() {
    let fixtures = fixtures();
    assert_eq!(fixtures.len(), 5);
    for f in fixtures {
        assert_eq!(f.schema_version, 1);
        assert!(!f.atlas_fixture_ids.is_empty());
        let roundtrip: Fixture = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(roundtrip, f);
    }
}

use bss_pricing::{
    api::{
        pricing_acceptance::PricingAcceptanceProvider, pricing_read::PricingReadProvider,
        sellability::SellabilityProvider,
    },
    config::SellerHoldPolicy,
    domain::{money, price_book_entry::Model as DomainModel},
    infra::{clock::Clock, commercial_terms::CommercialTermsService},
};
use bss_pricing_sdk::{acceptance::*, digest::*, read::*, terms::*};
use plan_support as p;
use rust_decimal::Decimal;
use seam_support as s;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

impl Commercial {
    fn policy(&self) -> UsageRatingPolicyInput {
        (&wire::UsageRatingPolicyInput {
            rating_window: self.rating_window.clone(),
            aggregation_scope: self.aggregation_scope,
            reset: self.reset,
            partial_window: self.partial_window,
            fold: self.quantity_semantics.fold,
        })
            .into()
    }
    fn domain_model(&self) -> DomainModel {
        match self.model {
            Model::PerUnit => DomainModel::PerUnit,
            Model::Volume => DomainModel::Volume,
            Model::Graduated => DomainModel::Graduated,
        }
    }
    fn money(&self) -> serde_json::Value {
        match self.model {
            Model::PerUnit => json!({"rate":self.rate.as_ref().unwrap()}),
            Model::Volume | Model::Graduated => json!({"tiers": self.tiers}),
        }
    }
    fn price_model(&self) -> PriceModel {
        let tiers = || {
            self.tiers
                .iter()
                .map(|t| bss_pricing_sdk::read::Tier {
                    up_to: t.up_to.as_ref().map(|s| s.parse().unwrap()),
                    rate: t.rate.parse().unwrap(),
                })
                .collect()
        };
        match self.model {
            Model::PerUnit => PriceModel::PerUnit {
                unit_amount: self.rate.as_ref().unwrap().parse().unwrap(),
            },
            Model::Volume => PriceModel::Volume { tiers: tiers() },
            Model::Graduated => PriceModel::Graduated { tiers: tiers() },
        }
    }
    fn amount(&self, quantity: &str) -> Decimal {
        let model = self.domain_model();
        let data = money::decode(model, self.money()).unwrap();
        money::amount_for(model, &data, quantity.parse().unwrap()).unwrap()
    }
}
fn volume_amount(quantity: &str) -> Decimal {
    let data = money::decode(
        DomainModel::Volume,
        json!({
            "tiers":[{"up_to":"10","rate":"0.02"},{"up_to":null,"rate":"0.015"}]
        }),
    )
    .unwrap();
    money::amount_for(DomainModel::Volume, &data, quantity.parse().unwrap()).unwrap()
}
#[test]
fn hourly_volume_fixture_requires_two_independent_quantities() {
    // Catalog representation sanity only. Rating's scheduler and Billing's invoice are external.
    let first = volume_amount("8");
    let second = volume_amount("12");
    assert_eq!(first + second, "0.34".parse::<Decimal>().unwrap());
    assert_eq!(volume_amount("20"), "0.30".parse::<Decimal>().unwrap());
    assert_ne!(first + second, volume_amount("20"));
}
#[test]
fn f02_f23_f24_exact_money_and_threshold_vectors() {
    for f in fixtures() {
        for a in f.expected.arithmetic {
            assert_eq!(
                f.given.commercial.amount(&a.quantity),
                a.amount.parse::<Decimal>().unwrap(),
                "{:?} Q={}",
                f.scenario,
                a.quantity
            );
        }
    }
}
#[derive(Deserialize)]
struct Identity {
    id: Uuid,
}
#[derive(Deserialize)]
struct CreatedPrices {
    items: Vec<Identity>,
}
#[derive(Deserialize)]
struct Applied {
    applied: bool,
}
#[derive(Deserialize)]
struct Published {
    revision: RevisionState,
}
#[derive(Deserialize)]
struct RevisionState {
    state: String,
}
#[derive(Deserialize)]
struct EntryView {
    id: Uuid,
    usage_rating_policy: wire::UsageRatingPolicy,
}

struct FixedClock(parking_lot::Mutex<time::OffsetDateTime>);
impl Clock for FixedClock {
    fn now(&self) -> time::OffsetDateTime {
        *self.0.lock()
    }
}
struct World {
    read: Arc<dyn PricingReadV1>,
    sell: Arc<dyn SellabilityV1>,
    acceptance: Arc<dyn PricingAcceptanceV1>,
    f: p::Fixture,
    catalog: Arc<p::Catalog>,
    clock: Arc<FixedClock>,
    book: Uuid,
    at: time::OffsetDateTime,
}
impl World {
    async fn new() -> Self {
        let (f, catalog) = p::setup().await;
        for kind in ["prices", "plan_revision"] {
            let tag = f
                .call("GET", "/approval-policy", json!({}), None, None)
                .await
                .2;
            let result = f
                .call(
                    "PUT",
                    "/approval-policy",
                    json!({"kind":kind,"quorum":0}),
                    Some(&tag),
                    None,
                )
                .await;
            assert_eq!(result.0, 200, "{result:?}");
        }
        let book = p::book(&f, "SEAM").await;
        // Actual authoring uses today's date; the injected commercial clock shares that day.
        let at = time::OffsetDateTime::now_utc()
            .date()
            .with_hms(10, 0, 0)
            .unwrap()
            .assume_utc();
        let clock = Arc::new(FixedClock(parking_lot::Mutex::new(at)));
        let enforcer = Arc::new(p::entry_support::enforcer_for(f.ctx.subject_tenant_id()));
        let service = Arc::new(CommercialTermsService::new(
            f.state.clone(),
            enforcer.clone(),
            clock.clone(),
            SellerHoldPolicy::default(),
        ));
        let hub = &f.state.hub;
        hub.register::<dyn PricingReadV1>(Arc::new(PricingReadProvider::new(
            f.state.clone(),
            enforcer,
        )));
        hub.register::<dyn SellabilityV1>(Arc::new(SellabilityProvider::new(service.clone())));
        hub.register::<dyn PricingAcceptanceV1>(Arc::new(PricingAcceptanceProvider::new(service)));
        Self {
            read: hub.get::<dyn PricingReadV1>().unwrap(),
            sell: hub.get::<dyn SellabilityV1>().unwrap(),
            acceptance: hub.get::<dyn PricingAcceptanceV1>().unwrap(),
            f,
            catalog,
            clock,
            book,
            at,
        }
    }
    fn catalog_ref(&self) -> CatalogRef {
        CatalogRef {
            tenant_id: self.f.ctx.subject_tenant_id(),
        }
    }
    async fn create_entry(&self, book: Uuid, sku: Uuid, c: &Commercial) -> EntryView {
        let r = self.f.call("POST", &format!("/price-books/{book}/entries"),
            json!({"sku_id":sku,"model":c.model,"usage_rating_policy":wire::UsageRatingPolicyInput::from(&c.policy())}), None, Some(&Uuid::new_v4().to_string())).await;
        assert_eq!(r.0, 201, "{r:?}");
        serde_json::from_value(r.1).unwrap()
    }
    async fn approve_price(&self, entry: Uuid, c: &Commercial, date: time::Date) -> Uuid {
        let r = self.f.call("POST", &format!("/price-book-entries/{entry}/prices"),
            json!({"price":c.money(),"min_fee":c.minimum_fee,"eligibility":"all","effective_from":date.to_string()}), None, Some(&Uuid::new_v4().to_string())).await;
        assert_eq!(r.0, 201, "{r:?}");
        let created: CreatedPrices = serde_json::from_value(r.1).unwrap();
        assert_eq!(created.items.len(), 1);
        let id = created.items[0].id;
        let r = self
            .f
            .call(
                "POST",
                &format!("/prices/{id}/submit"),
                json!({}),
                None,
                Some(&Uuid::new_v4().to_string()),
            )
            .await;
        assert_eq!(r.0, 201, "{r:?}");
        assert!(serde_json::from_value::<Applied>(r.1).unwrap().applied);
        id
    }
    async fn author(&self, f: &Fixture) -> AcceptedBinding {
        let c = &f.given.commercial;
        let d = &f.given.descriptors;
        let sku = self.catalog.sku(bss_products_sdk::models::SkuType::Usage);
        {
            let mut skus = self.catalog.skus.lock().unwrap();
            let s = skus.get_mut(&sku).unwrap();
            s.meter = Some(c.quantity_semantics.usage_type_id.clone());
            s.unit = Some(c.quantity_semantics.unit.clone());
        }
        let mut content = self.catalog.content(sku);
        content.code = d.code.clone();
        content.name = d.name.clone();
        content.gl_code = Some(d.gl_code.clone());
        content.tax_category = Some(d.tax_category.clone());
        content.invoice_line_template = Some(d.template.clone());
        content.billing_timing = Some(bss_products_sdk::models::BillingTiming::Arrears);
        self.catalog
            .version(sku, d.sku_version, &self.at.date().to_string(), content);
        let entry = self.create_entry(self.book, sku, c).await;
        let id = self.approve_price(entry.id, c, self.at.date()).await;
        let e = &f.expected;
        // Expected pins are built from fixture values and authoring identities, never copied from resolve.
        let mut price = ImmutablePrice {
            price_id: id,
            price_book_entry_id: entry.id,
            money_digest: [0; 32],
            currency: "EUR".into(),
            model: e.commercial.price_model(),
            minimum_fee: e
                .commercial
                .minimum_fee
                .as_ref()
                .map(|x| x.parse().unwrap()),
            effective_from: self.at.date(),
            ends_on: None,
            state: bss_pricing_sdk::read::PriceState::Approved,
        };
        price.money_digest = money_digest(&price);
        let policy = entry.usage_rating_policy.typed().unwrap();
        assert_eq!(policy.content, e.commercial.policy());
        assert_eq!(policy.version, 1);
        assert_eq!(policy.digest, policy_digest(&e.commercial.policy()));
        let d = &e.descriptors;
        AcceptedBinding {
            item_id: Uuid::nil(),
            price_book_entry_id: entry.id,
            dimension_key: None,
            dimension_value: None,
            sku_id: sku,
            sku_version: d.sku_version,
            sku_code: d.code.clone(),
            sku_name: d.name.clone(),
            unit: Some(e.commercial.quantity_semantics.unit.clone()),
            meter: Some(MeterRef {
                usage_type_id: e.commercial.quantity_semantics.usage_type_id.clone(),
                version: e.commercial.quantity_semantics.usage_type_version.clone(),
            }),
            price,
            kind: ChargeKind::Usage,
            recurring_period: e
                .commercial
                .entry_period
                .as_ref()
                .map(|v| v.parse().unwrap()),
            via_default: false,
            usage_rating_policy: Some(policy),
            invoice: InvoiceInputs {
                template: d.template.clone(),
                template_digest: template_digest(&d.template),
                template_source: InputSource::SkuVersion,
                gl_code: d.gl_code.clone(),
                tax_category: d.tax_category.clone(),
                timing: BillingTiming::Arrears,
                currency_scale: 2,
                rounding: Rounding::HalfEven,
            },
        }
    }
    async fn publish(&self, bindings: &mut [AcceptedBinding]) -> NewSaleQuery {
        let (plan, revision_id) = p::plan(
            &self.f,
            &Uuid::new_v4().simple().to_string().to_uppercase(),
            self.book,
        )
        .await;
        let plan: Identity = serde_json::from_value(plan).unwrap();
        for b in bindings.iter_mut() {
            b.item_id = p::item(
                &self.f,
                revision_id,
                b.sku_id,
                Some(b.price_book_entry_id),
                "paid",
            )
            .await
            .id;
        }
        let r = self
            .f
            .call(
                "POST",
                &format!("/plan-revisions/{revision_id}/submit"),
                json!({}),
                None,
                Some(&Uuid::new_v4().to_string()),
            )
            .await;
        assert_eq!(r.0, 201, "{r:?}");
        assert_eq!(
            serde_json::from_value::<Published>(r.1)
                .unwrap()
                .revision
                .state,
            "published"
        );
        let mut q = s::sale_query();
        q.tenant_axes.seller_tenant_id = self.f.ctx.subject_tenant_id();
        q.plan_id = plan.id;
        q.plan_revision_id = revision_id;
        q.order_id = Uuid::new_v4();
        q.line_id = Uuid::new_v4();
        q.start_at = self.at;
        q.billing_terms.anchor_at = self
            .at
            .date()
            .replace_day(1)
            .unwrap()
            .midnight()
            .assume_utc();
        q.billing_terms.digest = billing_terms_digest(&q.billing_terms);
        let resolved = self.resolve(&q).await;
        q.selections = resolved.cells.iter().map(|c| c.selection.clone()).collect();
        q.resolved_bindings_digest = selected_bindings_digest(&resolved, &q.selections).unwrap();
        compare_bindings(
            &resolved
                .cells
                .into_iter()
                .map(|c| c.binding.unwrap())
                .collect::<Vec<_>>(),
            bindings,
        );
        q
    }
    async fn resolve(&self, q: &NewSaleQuery) -> ResolvedBindings {
        self.read
            .resolve(
                &self.f.ctx,
                ResolveQuery {
                    catalog: self.catalog_ref(),
                    revision_id: q.plan_revision_id,
                    date: q.start_at.date(),
                    item_id: None,
                    pins: vec![],
                },
            )
            .await
            .unwrap()
    }
    async fn exercise(&self, q: &NewSaleQuery, expected: &[AcceptedBinding]) -> AcceptanceReceipt {
        let revision = self
            .read
            .current_revision(
                &self.f.ctx,
                PlanQuery {
                    catalog: self.catalog_ref(),
                    plan_id: q.plan_id,
                },
            )
            .await
            .unwrap();
        assert_eq!(revision.revision_id, q.plan_revision_id);
        for b in expected {
            assert_eq!(
                self.read
                    .price(
                        &self.f.ctx,
                        PriceQuery {
                            catalog: self.catalog_ref(),
                            price_id: b.price.price_id
                        }
                    )
                    .await
                    .unwrap(),
                b.price
            );
        }
        let before = s::written(&self.f).await;
        let refs = self.catalog.refs.lock().unwrap().clone();
        let a = self
            .sell
            .check(
                &self.f.ctx,
                q.clone(),
                meta(&format!("accept-{}", q.line_id)),
            )
            .await
            .unwrap();
        compare_bindings(&a.bindings, expected);
        assert_receipt(&a, q, *self.clock.0.lock());
        assert_eq!(
            self.acceptance
                .acceptance(
                    &self.f.ctx,
                    AcceptanceQuery {
                        catalog: self.catalog_ref(),
                        acceptance_id: a.acceptance_id
                    }
                )
                .await
                .unwrap(),
            a
        );
        let fq = fulfilment(&a);
        let eligibility = self
            .sell
            .check_fulfilment(&self.f.ctx, fq.clone())
            .await
            .unwrap();
        assert_eq!(eligibility.valid_before, a.hold_until);
        let held = self
            .acceptance
            .hold(&self.f.ctx, fq, meta(&format!("hold-{}", q.line_id)))
            .await
            .unwrap();
        compare_bindings(&held.bindings, expected);
        assert_eq!(held.terms_digest, a.terms_digest);
        assert_eq!(held.acceptance_id, a.acceptance_id);
        assert_eq!(held.activation_at, q.start_at);
        let after = s::written(&self.f).await;
        assert_eq!(
            after[2..],
            before[2..],
            "acceptance/hold emit no outbox work or reference operation"
        );
        let held_refs = self.catalog.refs.lock().unwrap().clone();
        assert_eq!(held_refs, refs, "acceptance provisions nothing");
        a
    }
}
fn assert_receipt(a: &AcceptanceReceipt, q: &NewSaleQuery, now: time::OffsetDateTime) {
    assert_eq!(a.query, *q);
    assert_eq!(a.request_digest, request_digest(q));
    assert_eq!(a.terms_digest, terms_digest(q, &a.bindings));
    assert_eq!(a.accepted_at, now);
    assert_eq!(a.hold_until - a.accepted_at, time::Duration::hours(24));
}
fn meta(key: &str) -> CommandMeta {
    CommandMeta {
        idempotency_key: key.into(),
    }
}
fn fulfilment(a: &AcceptanceReceipt) -> FulfilmentQuery {
    FulfilmentQuery {
        tenant_axes: a.query.tenant_axes.clone(),
        acceptance: AcceptanceRef {
            acceptance_id: a.acceptance_id,
            terms_digest: a.terms_digest,
        },
        current_market: a.query.market.clone(),
        activation_at: a.query.start_at,
    }
}
use seam_support::compare_bindings;
#[tokio::test]
async fn vm_and_hourly_fixtures_round_trip_all_c01_providers() {
    for f in fixtures().into_iter().take(3) {
        let w = World::new().await;
        let mut expected = vec![w.author(&f).await];
        let mut q = w.publish(&mut expected).await;
        q.billing_terms.cycle = f.given.commercial.billing_cycle.parse().unwrap();
        assert_eq!(
            q.billing_terms.cycle,
            f.expected.commercial.billing_cycle.parse().unwrap()
        );
        q.billing_terms.digest = billing_terms_digest(&q.billing_terms);
        w.exercise(&q, &expected).await;
    }
}

#[tokio::test]
async fn mixed_windows_and_two_plans_reuse_policy_without_pooling_lines() {
    let fixtures = fixtures();
    let w = World::new().await;
    let vm = w.author(&fixtures[0]).await;
    let cloud = w.author(&fixtures[1]).await;
    let mut mixed = vec![vm, cloud.clone()];
    let q1 = w.publish(&mut mixed).await;
    let a1 = w.exercise(&q1, &mixed).await;
    let mut reused = vec![cloud];
    let q2 = w.publish(&mut reused).await;
    let a2 = w.exercise(&q2, &reused).await;
    let first = a1
        .bindings
        .iter()
        .find(|b| b.price_book_entry_id == reused[0].price_book_entry_id)
        .unwrap();
    assert_eq!(
        first.usage_rating_policy,
        a2.bindings[0].usage_rating_policy
    );
    assert_ne!(a1.query.plan_id, a2.query.plan_id);
    assert_ne!(a1.query.line_id, a2.query.line_id);
    assert_ne!(first.item_id, a2.bindings[0].item_id);
    assert_eq!(
        first
            .usage_rating_policy
            .as_ref()
            .unwrap()
            .content
            .aggregation_scope,
        AggregationScope::SubscriptionLine
    );
    // Closed policy JSON has no runtime subscription/group key. Consumers derive separate line keys.
    let policy = serde_json::to_string(&wire::UsageRatingPolicyInput::from(
        &first.usage_rating_policy.as_ref().unwrap().content,
    ))
    .unwrap();
    for key in ["subscription_id", "aggregation_key", "pool_id"] {
        assert!(!policy.contains(key));
    }
}

#[tokio::test]
async fn f07_published_successor_preserves_all_accepted_pins() {
    let fixture = fixtures().remove(3);
    let world = World::new().await;
    let mut expected = vec![world.author(&fixture).await];
    let query = world.publish(&mut expected).await;
    let receipt = world
        .sell
        .check(&world.f.ctx, query.clone(), meta("frozen"))
        .await
        .unwrap();
    compare_bindings(&receipt.bindings, &expected);
    let mut successor = fixture.given.commercial.clone();
    successor.rate = fixture.given.successor_rate.clone();
    let tomorrow = world.at.date() + time::Duration::days(1);
    let next = world
        .approve_price(expected[0].price_book_entry_id, &successor, tomorrow)
        .await;
    let mut content = world.catalog.content(expected[0].sku_id);
    content.name = "SKU v4".into();
    content.gl_code = Some("NEW_REVENUE".into());
    content.tax_category = Some("new-tax".into());
    content.invoice_line_template = Some("New usage".into());
    content.billing_timing = Some(bss_products_sdk::models::BillingTiming::Arrears);
    world
        .catalog
        .version(expected[0].sku_id, 4, &tomorrow.to_string(), content);
    *world.clock.0.lock() += time::Duration::minutes(3);
    let mut fq = fulfilment(&receipt);
    fq.activation_at = *world.clock.0.lock();
    let held = world
        .acceptance
        .hold(&world.f.ctx, fq.clone(), meta("delayed"))
        .await
        .unwrap();
    compare_bindings(&held.bindings, &expected);
    assert_eq!(held.activation_at - world.at, time::Duration::minutes(3));
    assert_eq!(held.terms_digest, receipt.terms_digest);
    world
        .sell
        .check_fulfilment(&world.f.ctx, fq.clone())
        .await
        .unwrap();
    assert_eq!(
        world
            .acceptance
            .acceptance(
                &world.f.ctx,
                AcceptanceQuery {
                    catalog: world.catalog_ref(),
                    acceptance_id: receipt.acceptance_id
                }
            )
            .await
            .unwrap(),
        receipt
    );
    let renewed = world
        .read
        .resolve(
            &world.f.ctx,
            ResolveQuery {
                catalog: world.catalog_ref(),
                revision_id: query.plan_revision_id,
                date: tomorrow,
                item_id: None,
                pins: vec![PricePin {
                    item_id: expected[0].item_id,
                    dimension_value: None,
                    price_id: expected[0].price.price_id,
                }],
            },
        )
        .await
        .unwrap();
    let binding = renewed.cells[0].binding.as_ref().unwrap();
    assert_eq!(binding.price.price_id, next);
    assert_eq!(binding.price.model, successor.price_model());
    assert_eq!(binding.sku_version, 4);
    assert_eq!(binding.sku_name, "SKU v4");
    *world.clock.0.lock() = receipt.hold_until;
    assert_reason(
        &world
            .sell
            .check_fulfilment(&world.f.ctx, fq)
            .await
            .unwrap_err(),
        CommercialReason::HoldExpired,
    );
}
fn assert_reason(error: &toolkit_canonical_errors::CanonicalError, reason: CommercialReason) {
    assert_eq!(
        bss_pricing::infra::commercial_terms::errors::commercial_reason(error).as_deref(),
        Some(reason.as_str()),
        "{error:?}: {reason:?}"
    );
    let expected: toolkit_canonical_errors::CanonicalError = reason.into();
    assert_eq!(error.status_code(), expected.status_code());
}

#[tokio::test]
async fn entry_policy_mismatch_is_refused_and_book_remap_requires_exact_policy() {
    let f = fixtures().remove(1);
    let w = World::new().await;
    let original = w.author(&f).await;
    let mut bad = f.given.commercial.clone();
    bad.quantity_semantics.unit = "second".into();
    let mut policy =
        serde_json::to_value(wire::UsageRatingPolicyInput::from(&bad.policy())).unwrap();
    let q = &bad.quantity_semantics;
    policy["quantity_semantics"] = json!({
        "meter": {"usage_type_id": q.usage_type_id, "version": q.usage_type_version},
        "unit": q.unit,
        "fold": "SUM",
        "accrual_policy_version": q.accrual_policy_version
    });
    let result =
        w.f.call(
            "POST",
            &format!("/price-books/{}/entries", w.book),
            json!({"sku_id":original.sku_id,"model":"volume","usage_rating_policy":policy}),
            None,
            Some("mismatch"),
        )
        .await;
    assert_eq!(result.0, 400, "{result:?}");
    let problem: Problem = serde_json::from_value(result.1).unwrap();
    assert!(
        problem
            .context
            .field_violations
            .iter()
            .any(|v| v.reason == "METER_POLICY_MISMATCH"),
        "{problem:?}"
    );
    let target = p::book(&w.f, "TARGET").await;
    let mut different = f.given.commercial.clone();
    different.rating_window = wire::RatingWindow::BillingCycle;
    let wrong = w.create_entry(target, original.sku_id, &different).await;
    let exact = w
        .create_entry(target, original.sku_id, &f.given.commercial)
        .await;
    assert_ne!(
        wrong.usage_rating_policy.digest,
        exact.usage_rating_policy.digest
    );
    assert_eq!(
        exact.usage_rating_policy.typed().unwrap(),
        original.usage_rating_policy.clone().unwrap()
    );
    let price = w
        .approve_price(exact.id, &f.given.commercial, w.at.date())
        .await;
    let (_, revision) = p::plan(&w.f, "REMAP", w.book).await;
    let item = p::item(
        &w.f,
        revision,
        original.sku_id,
        Some(original.price_book_entry_id),
        "paid",
    )
    .await;
    let path = format!("/plan-revisions/{revision}");
    let tag = w.f.call("GET", &path, json!({}), None, None).await.2;
    let result =
        w.f.call("PATCH", &path, json!({"book_id":target}), Some(&tag), None)
            .await;
    assert_eq!(result.0, 200, "{result:?}");
    assert_eq!(
        p::items(&w.f, revision).await[0].price_book_entry_id,
        Some(exact.id)
    );
    let result =
        w.f.call(
            "POST",
            &format!("{path}/submit"),
            json!({}),
            None,
            Some("remap"),
        )
        .await;
    assert_eq!(result.0, 201, "{result:?}");
    let resolved = w
        .read
        .resolve(
            &w.f.ctx,
            ResolveQuery {
                catalog: w.catalog_ref(),
                revision_id: revision,
                date: w.at.date(),
                item_id: None,
                pins: vec![],
            },
        )
        .await
        .unwrap();
    let mut expected = original;
    expected.item_id = item.id;
    expected.price_book_entry_id = exact.id;
    expected.price.price_book_entry_id = exact.id;
    expected.price.price_id = price;
    expected.price.money_digest = money_digest(&expected.price);
    compare_bindings(&[resolved.cells[0].binding.clone().unwrap()], &[expected]);
}
/// Typed canonical field violations at the authoring boundary.
#[derive(Debug, Deserialize)]
struct Problem {
    context: ProblemContext,
}
#[derive(Debug, Deserialize)]
struct ProblemContext {
    field_violations: Vec<Violation>,
}
#[derive(Debug, Deserialize)]
struct Violation {
    reason: String,
}

#[tokio::test]
async fn all_seven_clienthub_calls_deny_unauthorized_and_system_named_callers() {
    let f = fixtures().remove(0);
    let w = World::new().await;
    let mut bindings = vec![w.author(&f).await];
    let q = w.publish(&mut bindings).await;
    let a = w.exercise(&q, &bindings).await;
    for (ctx, status) in [
        (p::holding(&w.f, "denied"), 403),
        (p::holding(&w.f, "bss-orders.system"), 403),
        (toolkit_security::SecurityContext::anonymous(), 401),
    ] {
        let before = s::written(&w.f).await;
        let errors = [
            w.read
                .resolve(
                    &ctx,
                    ResolveQuery {
                        catalog: w.catalog_ref(),
                        revision_id: q.plan_revision_id,
                        date: w.at.date(),
                        item_id: None,
                        pins: vec![],
                    },
                )
                .await
                .unwrap_err(),
            w.read
                .price(
                    &ctx,
                    PriceQuery {
                        catalog: w.catalog_ref(),
                        price_id: bindings[0].price.price_id,
                    },
                )
                .await
                .unwrap_err(),
            w.read
                .current_revision(
                    &ctx,
                    PlanQuery {
                        catalog: w.catalog_ref(),
                        plan_id: q.plan_id,
                    },
                )
                .await
                .unwrap_err(),
            w.sell
                .check(&ctx, q.clone(), meta("accept"))
                .await
                .unwrap_err(),
            w.sell
                .check_fulfilment(&ctx, fulfilment(&a))
                .await
                .unwrap_err(),
            w.acceptance
                .acceptance(
                    &ctx,
                    AcceptanceQuery {
                        catalog: w.catalog_ref(),
                        acceptance_id: a.acceptance_id,
                    },
                )
                .await
                .unwrap_err(),
            w.acceptance
                .hold(&ctx, fulfilment(&a), meta("hold"))
                .await
                .unwrap_err(),
        ];
        for error in errors {
            assert_eq!(error.status_code(), status, "{error:?}");
        }
        assert_eq!(s::written(&w.f).await, before);
    }
}

#[tokio::test]
async fn f22_f31_unsupported_inputs_fail_before_acceptance() {
    use bss_pricing::infra::commercial_terms_wire::decode_billing_terms;
    let f = fixtures().remove(4);
    for refusal in &f.expected.refusals {
        let w = World::new().await;
        let mut bindings = vec![w.author(&f).await];
        let mut q = w.publish(&mut bindings).await;
        let before = receipt_count(&w).await;
        match refusal {
            Refusal::MissingPolicy | Refusal::MinimumFee | Refusal::Package => {
                // Legacy/corrupt catalog rows cannot be authored through the modern gates.
                // Inject them after publication to prove the actual sellability provider fails closed.
                match refusal {
                    Refusal::MissingPolicy => execute(&w,"UPDATE pricing_price_book_entry SET usage_policy_id=NULL,usage_policy_version=NULL,usage_policy_digest=NULL,usage_sku_version=NULL").await,
                    Refusal::MinimumFee => execute(&w,"UPDATE pricing_price SET min_fee='0'").await,
                    Refusal::Package => {
                        execute(&w,"UPDATE pricing_price_book_entry SET model='package'").await;
                        execute(&w,r#"UPDATE pricing_price SET price_json='{"package_size":"10","package_price":"1"}'"#).await;
                    }
                    _=>unreachable!(),
                }
                let resolved = w.resolve(&q).await;
                q.resolved_bindings_digest =
                    selected_bindings_digest(&resolved, &q.selections).unwrap();
                let reason = match refusal {
                    Refusal::MissingPolicy => CommercialReason::MissingRatingPolicy,
                    Refusal::Package => CommercialReason::UnsupportedModel,
                    _ => CommercialReason::UnsupportedTerms,
                };
                assert_reason(
                    &w.sell
                        .check(&w.f.ctx, q, meta("unsupported"))
                        .await
                        .unwrap_err(),
                    reason,
                );
            }
            Refusal::UnalignedAnchor => {
                q.billing_terms.anchor = BillingAnchor::SubscriptionStart;
                q.billing_terms.anchor_at = w.at + time::Duration::minutes(30);
                q.billing_terms.digest = billing_terms_digest(&q.billing_terms);
                assert_reason(
                    &w.sell
                        .check(&w.f.ctx, q, meta("unsupported"))
                        .await
                        .unwrap_err(),
                    CommercialReason::UnalignedBillingAnchor,
                );
            }
            Refusal::MissingBillingTerms => assert_eq!(
                decode_billing_terms("null").unwrap_err().reason,
                CommercialReason::MissingBillingTerms
            ),
            Refusal::Quarterly => assert_eq!(
                "quarter".parse::<BillingCycle>().unwrap_err().reason,
                CommercialReason::UnsupportedTerms
            ),
            Refusal::CrossLinePooling | Refusal::IncludedQuantity => {
                let mut input = serde_json::to_value(wire::UsageRatingPolicyInput::from(
                    &f.given.commercial.policy(),
                ))
                .unwrap();
                if *refusal == Refusal::CrossLinePooling {
                    input["aggregation_scope"] = json!("cross_subscription");
                } else {
                    input["included_quantity"] = json!("1");
                }
                let result=w.f.call("POST",&format!("/price-books/{}/entries",w.book),
                    json!({"sku_id":bindings[0].sku_id,"model":"volume","usage_rating_policy":input}),None,Some("unsupported")).await;
                assert_eq!(result.0, 400, "{refusal:?}: {result:?}");
                assert!(serde_json::from_value::<wire::UsageRatingPolicyInput>(input).is_err());
            }
            Refusal::Phases | Refusal::Promotions | Refusal::Fx => {
                let mut terms = json!({"schema_version":"1","cycle":"month","anchor":"calendar","anchor_at":"2026-10-01T00:00:00Z","timezone":"UTC","source":{"kind":"explicit_order"},"digest":"00".repeat(32)});
                let key = match refusal {
                    Refusal::Phases => "phases",
                    Refusal::Promotions => "promotions",
                    _ => "fx",
                };
                terms[key] = json!(true);
                assert_eq!(
                    decode_billing_terms(&terms.to_string()).unwrap_err().reason,
                    CommercialReason::UnsupportedTerms
                );
            }
        }
        // These five are wire-adapter vectors with no production transport or command;
        // checking receipt_count for them cannot observe a side effect.
        if !matches!(
            refusal,
            Refusal::MissingBillingTerms
                | Refusal::Quarterly
                | Refusal::Phases
                | Refusal::Promotions
                | Refusal::Fx
        ) {
            assert_eq!(
                receipt_count(&w).await,
                before,
                "{refusal:?} creates no acceptance"
            );
        }
    }
}
async fn execute(w: &World, sql: &str) {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    Database::connect(&w.f.dsn)
        .await
        .unwrap()
        .execute_raw(Statement::from_string(DbBackend::Sqlite, sql))
        .await
        .unwrap();
}
async fn receipt_count(w: &World) -> i64 {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};
    Database::connect(&w.f.dsn)
        .await
        .unwrap()
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT count(*) AS n FROM pricing_acceptance",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "n")
        .unwrap()
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
enum RestMoney {
    PerUnit { rate: String },
    Tiered { tiers: Vec<Tier> },
}
impl RestMoney {
    fn model(&self, kind: &Model) -> PriceModel {
        match (self, kind) {
            (Self::PerUnit { rate }, Model::PerUnit) => PriceModel::PerUnit {
                unit_amount: rate.parse().unwrap(),
            },
            (Self::Tiered { tiers }, Model::Volume | Model::Graduated) => {
                let tiers = tiers
                    .iter()
                    .map(|t| bss_pricing_sdk::read::Tier {
                        up_to: t.up_to.as_ref().map(|v| v.parse().unwrap()),
                        rate: t.rate.parse().unwrap(),
                    })
                    .collect();
                if *kind == Model::Volume {
                    PriceModel::Volume { tiers }
                } else {
                    PriceModel::Graduated { tiers }
                }
            }
            _ => panic!("REST money/model mismatch"),
        }
    }
}
#[derive(Deserialize)]
struct RestPrice {
    price_id: Uuid,
    price_book_entry_id: Uuid,
    sku_id: Uuid,
    model: Model,
    price: RestMoney,
    currency: String,
    period: Option<String>,
    min_fee: Option<String>,
}
#[derive(Deserialize)]
struct RestResolve {
    plan_id: Uuid,
    plan_revision_id: Uuid,
    items: Vec<RestItem>,
}
#[derive(Deserialize)]
struct RestItem {
    item_id: Uuid,
    sku_id: Uuid,
    price_book_entry_id: Uuid,
    usage_rating_policy: Option<wire::UsageRatingPolicy>,
    model: Model,
    period: Option<String>,
    chains: Vec<RestChain>,
}
#[derive(Deserialize)]
struct RestChain {
    binding: RestBinding,
}
#[derive(Deserialize)]
struct RestBinding {
    price_id: Uuid,
    price: RestMoney,
    min_fee: Option<String>,
}
#[tokio::test]
async fn rest_reads_preserve_money_and_add_only_optional_entry_policy() {
    for f in fixtures().into_iter().take(3) {
        let w = World::new().await;
        let mut bindings = vec![w.author(&f).await];
        let q = w.publish(&mut bindings).await;
        let b = &bindings[0];
        let r =
            w.f.call(
                "GET",
                &format!("/prices/{}", b.price.price_id),
                json!({}),
                None,
                None,
            )
            .await;
        assert_eq!(r.0, 200, "{r:?}");
        let price: RestPrice = serde_json::from_value(r.1).unwrap();
        assert_eq!(
            (price.price_id, price.price_book_entry_id, price.sku_id),
            (b.price.price_id, b.price_book_entry_id, b.sku_id)
        );
        assert_eq!(price.price.model(&price.model), b.price.model);
        assert_eq!(price.currency, b.price.currency);
        assert_eq!(price.period, None);
        assert_eq!(price.min_fee, None);
        let (status, body) = s::resolve(
            &w.f,
            &format!(
                "plan_revision_id={}&date={}",
                q.plan_revision_id,
                w.at.date()
            ),
        )
        .await;
        assert_eq!(status, 200);
        let rest: RestResolve = serde_json::from_value(body).unwrap();
        assert_eq!(
            (rest.plan_id, rest.plan_revision_id),
            (q.plan_id, q.plan_revision_id)
        );
        assert_eq!(rest.items.len(), 1);
        let item = &rest.items[0];
        assert_eq!(
            (item.item_id, item.sku_id, item.price_book_entry_id),
            (b.item_id, b.sku_id, b.price_book_entry_id)
        );
        assert_eq!(
            item.usage_rating_policy
                .as_ref()
                .map(|p| p.typed().unwrap()),
            b.usage_rating_policy
        );
        assert_eq!(item.period, None);
        assert_eq!(item.chains.len(), 1);
        assert_eq!(item.chains[0].binding.price_id, b.price.price_id);
        assert_eq!(
            item.chains[0].binding.price.model(&item.model),
            b.price.model
        );
        assert_eq!(item.chains[0].binding.min_fee, None);
    }
}

#[test]
fn fixture_comparison_detects_a_dropped_policy_pin() {
    let f = fixtures().remove(0);
    let mut binding = s::vm_binding();
    binding.usage_rating_policy.as_mut().unwrap().content = f.expected.commercial.policy();
    let mut missing = binding.clone();
    missing.usage_rating_policy = None;
    assert!(
        std::panic::catch_unwind(|| compare_bindings(&[missing], &[binding])).is_err(),
        "the fixture comparison must observe its policy pin"
    );
}
#[test]
fn a_production_billing_window_rejects_an_unknown_variant() {
    let raw = r#"{"kind":"rolling"}"#;
    assert!(
        serde_json::from_str::<bss_pricing::infra::usage_policy_wire::RatingWindow>(raw).is_err()
    );
}
