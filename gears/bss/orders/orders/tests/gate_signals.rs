#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once; `allow`, not `expect`, because not every item unwraps or expects"
)]
//! Run-level signals one assessment contributes (DESIGN 03 §3.8; D-126, D-233).
use bss_orders::domain::dates::{
    DateBasis, DatePolicySnapshot, PLATFORM_DEFAULT_POLICY_ID, PolicyRow, SuppliedDates,
};
use bss_orders::domain::gate::answers::*;
use bss_orders::domain::gate::assess::{AssessmentInput, BasketLine, assess};
use bss_orders::domain::gate::ports::{PortAnswer, Unavailable};
use bss_orders::domain::gate::predicates::{Origin, PredicateId};
use bss_orders::domain::gate::pricing_map::*;
use bss_orders::domain::gate::signals::RunSignals;
use bss_orders::domain::gate::vector::{Operation, ProducerResult, Verdict};
use bss_orders_sdk::catalog::Reason;
use bss_orders_sdk::gate_report::CatalogSelection;
use bss_pricing_sdk::read::*;
use bss_pricing_sdk::terms::{BillingTiming, InputSource, InvoiceInputs, Rounding};
use bss_products_sdk::models::{Lifecycle, Sku, SkuType};
use std::collections::BTreeMap;
use std::sync::Arc;
use time::{Date, Month, OffsetDateTime};
use uuid::Uuid;

// ---- catalog fixtures (the Task 5 approach: full SDK values) ----

const PLAN: Uuid = Uuid::from_u128(1);
const REV: Uuid = Uuid::from_u128(2);
const ITEM: Uuid = Uuid::from_u128(3);
const SKU: Uuid = Uuid::from_u128(4);
const ITEM_B: Uuid = Uuid::from_u128(5);
const TENANT: Uuid = Uuid::from_u128(50);

fn line_id(n: u128) -> Uuid {
    Uuid::from_u128(100 + n)
}
fn day() -> Date {
    Date::from_calendar_date(2026, Month::October, 22).unwrap()
}
fn sel(item: Uuid, dim: Option<&str>, qty: &str) -> SelectedItem {
    SelectedItem {
        item_id: item,
        quantity: qty.into(),
        selected_dim_value: dim.map(str::to_owned),
    }
}
fn binding(item: Uuid) -> AcceptedBinding {
    AcceptedBinding {
        item_id: item,
        price_book_entry_id: Uuid::from_u128(20),
        dimension_key: None,
        dimension_value: None,
        sku_id: SKU,
        sku_version: 3,
        sku_code: "VM-SMALL".into(),
        sku_name: "Small VM".into(),
        unit: Some("instance".into()),
        meter: None,
        price: ImmutablePrice {
            price_id: Uuid::from_u128(21),
            price_book_entry_id: Uuid::from_u128(20),
            money_digest: [7; 32],
            currency: bss_money::CurrencySpec::try_new("USD".into(), 2).unwrap(),
            model: PriceModel::Flat {
                amount: bss_money::parse_decimal("10").unwrap(),
            },
            minimum_fee: None,
            effective_from: Date::from_calendar_date(2026, Month::January, 1).unwrap(),
            ends_on: None,
            state: PriceState::Approved,
        },
        kind: ChargeKind::Recurring,
        recurring_period: None,
        via_default: false,
        usage_rating_policy: None,
        invoice: InvoiceInputs {
            template: "{sku_name}".into(),
            template_digest: [8; 32],
            template_source: InputSource::Entry,
            gl_code: "4000".into(),
            tax_category: "standard".into(),
            timing: BillingTiming::Advance,
            rounding: Rounding::HalfEven,
        },
    }
}
fn cell(item: Uuid, dim: Option<&str>, bound: bool) -> ResolvedCell {
    ResolvedCell {
        selection: BindingSelection {
            item_id: item,
            dimension_value: dim.map(str::to_owned),
        },
        binding: bound.then(|| binding(item)),
    }
}
fn sku() -> Sku {
    let t = OffsetDateTime::UNIX_EPOCH;
    Sku {
        id: SKU,
        tenant_id: Uuid::from_u128(30),
        code: "VM-SMALL".into(),
        name: "Small VM".into(),
        r#type: SkuType::Recurring,
        category_id: None,
        description: "small".into(),
        sellable: true,
        lifecycle: Lifecycle::Published,
        retire_pending: false,
        lifecycle_next: None,
        revision: 1,
        published_version: 3,
        gl_code: None,
        tax_category: None,
        invoice_line_template: None,
        billing_timing: None,
        usage_type_ref: None,
        unit: None,
        type_change_pending: false,
        pending_unit_id: None,
        approved_by_unit_id: None,
        created_by: Uuid::from_u128(31),
        created_at: t,
        updated_at: t,
        archived_at: None,
        archived_by: None,
    }
}
/// An answer binding every slot the cells name.
fn catalog_answer(cells: Vec<ResolvedCell>) -> CatalogAnswer {
    CatalogAnswer {
        current: Ok(RevisionRef {
            plan_id: PLAN,
            revision_id: REV,
            revision_no: 1,
        }),
        resolved: Ok(Arc::new(ResolvedBindings {
            plan_id: PLAN,
            revision_id: REV,
            cells,
        })),
        skus: BTreeMap::from([(SKU, PortAnswer::Answered(sku()))]),
    }
}
fn good_catalog() -> CatalogAnswer {
    catalog_answer(vec![cell(ITEM, None, true), cell(ITEM_B, Some("x"), true)])
}

// ---- the other ports ----

fn profile() -> PayerProfile {
    PayerProfile {
        axes_valid: true,
        currency: "USD".into(),
        region: "US".into(),
        in_relationship_with_seller: true,
        in_relationship_with_resource: true,
        profile_version: "p1".into(),
    }
}
fn key_of(line: Uuid) -> String {
    format!("key-{}", line.as_u128())
}
fn overlap_keys(line: Uuid, key: &str) -> Vec<OverlapKey> {
    let selected = two_lines()
        .into_iter()
        .find(|l| l.line_id == line)
        .map_or_else(|| vec![sel(ITEM, None, "1")], |l| l.catalog.items);
    selected
        .into_iter()
        .map(|selection| {
            let identity = OverlapKeyLine {
                line_id: line,
                plan_revision_id: REV,
                item_id: selection.item_id,
                dimension_value: selection.selected_dim_value,
                paid_recurring_sku: SKU,
                sku_version: 3,
            };
            OverlapKey {
                line_id: line,
                catalog_subscription_product_key: format!("product-{key}"),
                overlap_scope_key: key.into(),
                item_key: Some(bss_orders::domain::gate::item_keys::ItemOverlapKey {
                    profile: "item_keys_v1".into(),
                    selection: identity,
                    catalog_subscription_product_key: format!("product-{key}"),
                    overlap_scope_key: key.into(),
                    classification: bss_orders::domain::gate::item_keys::ClassificationProvenance {
                        source: "test-provider".into(),
                        reference: "classification".into(),
                        version: "1".into(),
                    },
                }),
            }
        })
        .collect()
}

fn occupancy(
    key: &str,
    active: u32,
    max: MaxConcurrentActive,
) -> (OccupancyQuery, OccupancyObservation) {
    (
        OccupancyQuery {
            own_context: None,
            payer_tenant_id: TENANT,
            resource_tenant_id: Some(TENANT),
            overlap_scope_key: key.into(),
        },
        Occupancy {
            active_count: active,
            count_scope: bss_orders::domain::gate::answers::OccupancyCountScope::PerPayer,
            max_concurrent_active: max,
            provenance: LimitProvenance::Catalog,
        }
        .into(),
    )
}
fn terms(line: Uuid) -> ResolvedTerms {
    ResolvedTerms {
        line_id: line,
        billing_terms: BillingTermsRef {
            digest: "terms".into(),
        },
        term: EvaluationTerm::Rolling,
        earliest_start: None,
        provenance: TermsProvenance::PlatformDefault,
        snapshot: None,
    }
}
fn result(predicate: &str, verdict: Verdict, reason: Option<&str>) -> ProducerResult {
    ProducerResult {
        producer: "pricing".into(),
        profile: "assess/1".into(),
        predicate: predicate.into(),
        verdict,
        reason: reason.map(str::to_owned),
        detail: None,
    }
}
fn keyed(line: Uuid, slot: Option<&SelectedItem>, r: ProducerResult) -> ProducerResultWithKey {
    ProducerResultWithKey {
        line_id: line,
        item_id: slot.map(|s| s.item_id),
        selection: slot.map_or(CatalogSelection::NotSelected, |s| {
            s.selected_dim_value
                .clone()
                .map_or(CatalogSelection::SelectedNull, CatalogSelection::Selected)
        }),
        result: r,
    }
}
/// Passed results for every row the assessment port covers (predicates 2, 7, 8 and 4-Orders).
fn passing_assessment(lines: &[BasketLine]) -> AssessmentAnswer {
    let mut results = Vec::new();
    for l in lines {
        for p in [
            PredicateId::CatalogRevisionAvailable,
            PredicateId::CatalogEligibilityRules,
            PredicateId::MarketConsistency,
        ] {
            results.push(keyed(
                l.line_id,
                None,
                result(p.token(), Verdict::Passed, None),
            ));
        }
        for s in &l.catalog.items {
            let r = result(
                PredicateId::CatalogDimensionMarket.token(),
                Verdict::Passed,
                None,
            );
            results.push(keyed(l.line_id, Some(s), r));
        }
    }
    AssessmentAnswer {
        profile: bss_orders::domain::gate::orchestrate::ASSESSMENT_PROFILE.into(),
        observed_at: OffsetDateTime::UNIX_EPOCH,
        results,
    }
}
fn amount() -> Amount {
    Amount {
        exact: ExactMoney::new("10", "1", "USD", 2).unwrap(),
        display: "10.00".into(),
    }
}
fn evaluation() -> OrderEvaluation {
    let totals = Totals {
        gross: amount(),
        net: amount(),
        discount: Availability::Unavailable,
    };
    OrderEvaluation {
        assessment_id: Uuid::from_u128(60),
        estimate: true,
        currency: "USD".into(),
        currency_scale: 2,
        lines: Vec::new(),
        order_totals: totals,
        order_committed_components: None,
        order_tcv_carrier: None,
        order_declaration: None,
        order_usage_declaration: None,
        tcv: None,
        promotion_status: Availability::Unavailable,
        request_digest: "digest".into(),
    }
}
fn policy(sa_required: bool) -> DatePolicySnapshot {
    let row = PolicyRow {
        policy_id: PLATFORM_DEFAULT_POLICY_ID,
        resource_tenant_id: None,
        service_activation_required: sa_required,
        acceptance_due_required: sa_required,
        revision: 1,
    };
    DatePolicySnapshot::effective(TENANT, &[row]).unwrap()
}
fn dates_for(lines: &[BasketLine], sa_required: bool) -> DateBasis {
    let supplied = lines.iter().map(|l| (l.line_id, SuppliedDates::default()));
    DateBasis::resolve(TENANT, policy(sa_required), day(), supplied)
}
fn basket_line(n: u128, items: Vec<SelectedItem>) -> BasketLine {
    let id = line_id(n);
    BasketLine {
        line_id: id,
        catalog: LineCatalogRequest {
            line_id: id,
            plan_id: PLAN,
            revision_id: REV,
            items,
        },
        currency: Some("USD".into()),
        contract_ref: None,
    }
}

/// A Submit over `lines` where every port answers and every predicate passes.
fn input_for(op: Operation, lines: Vec<BasketLine>) -> AssessmentInput {
    let ids: Vec<Uuid> = lines.iter().map(|l| l.line_id).collect();
    AssessmentInput {
        occupancy_axes: None,
        unsupported_terms: std::collections::BTreeSet::default(),
        operation: op,
        resolve_date: day(),
        catalog: ids.iter().map(|id| (*id, good_catalog())).collect(),
        identity: PortAnswer::Answered(profile()),
        contract_id: None,
        contracts: PortAnswer::Answered(BTreeMap::new()),
        overlap_keys: PortAnswer::Answered(
            ids.iter()
                .flat_map(|id| overlap_keys(*id, &key_of(*id)))
                .collect(),
        ),
        occupancy: PortAnswer::Answered(
            ids.iter()
                .map(|id| occupancy(&key_of(*id), 0, MaxConcurrentActive::Limit(1)))
                .collect(),
        ),
        inflight_same_tuple: std::collections::BTreeSet::new(),
        inflight_other_orders: BTreeMap::new(),
        billing_terms: PortAnswer::Answered(ids.iter().map(|id| terms(*id)).collect()),
        acceptance_policy: PortAnswer::Answered(Some(AcceptancePolicy {
            version: "pol-1".into(),
            hold_seconds: 600,
        })),
        assessment: PortAnswer::Answered(passing_assessment(&lines)),
        evaluation: PortAnswer::Answered(evaluation()),
        dates: dates_for(&lines, false),
        lines,
    }
}
fn two_lines() -> Vec<BasketLine> {
    vec![
        basket_line(1, vec![sel(ITEM, None, "1")]),
        basket_line(2, vec![sel(ITEM, None, "2"), sel(ITEM_B, Some("x"), "0.5")]),
    ]
}
fn base(op: Operation) -> AssessmentInput {
    input_for(op, two_lines())
}
fn unavailable<T>() -> PortAnswer<T> {
    PortAnswer::Unavailable(Unavailable::Outage)
}

#[test]
fn a_passing_assessment_contributes_nothing() {
    let a = assess(&base(Operation::Submit)).unwrap();
    let s = RunSignals::of(&a);
    assert!(!s.blocked);
    assert!(s.refused.is_empty() && s.unevaluable.is_empty());
    assert_eq!(
        (
            s.pins_unresolvable,
            s.pins_unevaluable,
            s.occupancy_incomplete
        ),
        (0, 0, false)
    );
}

#[test]
fn a_refused_predicate_counts_once_per_run_not_per_line() {
    // Both lines carry an unsellable SKU: the adopted predicate 6 fails on each line.
    let mut input = base(Operation::Submit);
    for answer in input.catalog.values_mut() {
        if let Some(PortAnswer::Answered(s)) = answer.skus.get_mut(&SKU) {
            s.sellable = false;
        }
    }
    let a = assess(&input).unwrap();
    let failing_rows = a
        .vector
        .outcomes()
        .iter()
        .filter(|o| {
            o.key.predicate == PredicateId::CatalogSkuSellable && o.verdict == Verdict::Failed
        })
        .count();
    assert!(
        failing_rows >= 2,
        "the fixture must fail on more than one row"
    );
    let s = RunSignals::of(&a);
    assert!(s.blocked);
    let sellable: Vec<_> = s
        .refused
        .iter()
        .filter(|p| p.predicate == PredicateId::CatalogSkuSellable)
        .collect();
    assert_eq!(sellable.len(), 1, "several failing rows, one signal");
    assert_eq!(sellable[0].origin, Origin::Adopted);
    assert_eq!(sellable[0].reason, Reason::CatalogPredicateFailed);
}

#[test]
fn an_unavailable_assessment_port_contributes_unevaluable_rows_only() {
    let mut input = base(Operation::Submit);
    input.assessment = unavailable();
    let a = assess(&input).unwrap();
    let s = RunSignals::of(&a);
    assert!(
        s.unevaluable
            .iter()
            .any(|p| p.predicate == PredicateId::CatalogDimensionMarket
                && p.reason == Reason::CatalogPredicatesUnavailable)
    );
    assert!(
        s.refused
            .iter()
            .all(|p| p.predicate != PredicateId::CatalogDimensionMarket)
    );
    assert_eq!(
        (s.pins_unevaluable, s.pins_unresolvable),
        (0, 0),
        "the assessment port does not feed the pin"
    );
}

#[test]
fn an_unavailable_resolve_makes_each_lines_pin_unevaluable() {
    // `gate_assess.rs` `b_resolve`: `resolved = Err(Unavailable)` on a line's answer.
    let mut input = base(Operation::Submit);
    for answer in input.catalog.values_mut() {
        answer.resolved = Err(ReadFailure::Unavailable(Unavailable::Deadline));
    }
    let a = assess(&input).unwrap();
    let s = RunSignals::of(&a);
    assert_eq!((s.pins_unevaluable, s.pins_unresolvable), (2, 0));
    assert!(
        s.unevaluable.iter().any(|p| p.origin == Origin::Pin),
        "{s:?}"
    );
    assert!(s.refused.iter().all(|p| p.origin != Origin::Pin));
}

#[test]
fn an_incomplete_occupancy_answer_sets_the_d126_flag() {
    // An occupancy answer lacking `activeCount` or `maxConcurrentActive` is decoded as an
    // unavailable port (D-126; `gate_assess.rs` `b_occupancy` does the same).
    let mut input = base(Operation::Submit);
    input.occupancy = PortAnswer::Unavailable(Unavailable::Malformed);
    let a = assess(&input).unwrap();
    let s = RunSignals::of(&a);
    assert!(s.occupancy_incomplete);
    assert!(
        s.unevaluable
            .iter()
            .any(|p| p.predicate == PredicateId::OverlapUniqueness
                && p.reason == Reason::OverlapPresenceUnevaluable)
    );
}

#[test]
fn a_pin_that_cannot_compose_is_unresolvable() {
    // A selected slot with no binding on the current revision: the pin is unresolvable.
    let mut input = base(Operation::Submit);
    input
        .catalog
        .insert(line_id(1), catalog_answer(vec![cell(ITEM, None, false)]));
    let a = assess(&input).unwrap();
    let s = RunSignals::of(&a);
    assert_eq!(s.pins_unresolvable, 1);
    assert_eq!(s.pins_unevaluable, 0);
}
