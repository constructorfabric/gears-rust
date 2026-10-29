#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once; `allow`, not `expect`, because not every item unwraps or expects"
)]
//! The orchestrator's pure half (S3-11/S3-12; F03 §3.6 steps 2–5; D-215, D-180, M2-6): what one
//! run asks of each port, the identity gate, the occupancy dedup, the evaluation request and the
//! assembled assessment input.
use bss_orders::domain::dates::{
    DateBasis, DatePolicySnapshot, PLATFORM_DEFAULT_POLICY_ID, PolicyRow, SuppliedDates,
};
use bss_orders::domain::gate::answers::*;
use bss_orders::domain::gate::assess::{BasketLine, assess};
use bss_orders::domain::gate::commercial::CommercialError;
use bss_orders::domain::gate::orchestrate::{
    Answers, Basket, GateTarget, IdentityGate, LineTermIntent, LineTerms, TenantAxes, assemble,
    assessment_query, evaluation_request, gate_after_identity, line_bindings, line_keys,
    line_terms, occupancy_queries, overlap_key_lines, payer_market, plan, tax_query,
};
use bss_orders::domain::gate::ports::{PortAnswer, Unavailable};
use bss_orders::domain::gate::predicates::PredicateId;
use bss_orders::domain::gate::pricing_map::*;
use bss_orders::domain::gate::vector::{Operation, ProducerResult, Verdict};
use bss_orders::domain::guards::InputPort;
use bss_orders::domain::transition::InputState;
use bss_orders_sdk::authoring::{AuthoredTerm, BillingCycle};
use bss_orders_sdk::catalog::Reason;
use bss_orders_sdk::gate_report::CatalogSelection;
use bss_pricing_sdk::read::*;
use bss_pricing_sdk::terms::{BillingTiming, InputSource, InvoiceInputs, Rounding};
use bss_products_sdk::models::{Lifecycle, Sku, SkuType};
use std::collections::BTreeMap;
use std::sync::Arc;
use time::{Date, Month, OffsetDateTime};
use uuid::Uuid;

const PLAN: Uuid = Uuid::from_u128(1);
const REV: Uuid = Uuid::from_u128(2);
const ITEM: Uuid = Uuid::from_u128(3);
const SKU: Uuid = Uuid::from_u128(4);
const ITEM_B: Uuid = Uuid::from_u128(5);
const ONE_TIME_SKU: Uuid = Uuid::from_u128(6);
const RESOURCE: Uuid = Uuid::from_u128(50);
/// The order a Submit or Amendment fixture run belongs to.
const ORDER: Uuid = Uuid::from_u128(0x0de6);
const PAYER: Uuid = Uuid::from_u128(51);
const SELLER: Uuid = Uuid::from_u128(52);
const CONTRACT: Uuid = Uuid::from_u128(70);
const CONTRACT_B: Uuid = Uuid::from_u128(71);
const ASSESSMENT: Uuid = Uuid::from_u128(80);

/// `exact` with its currency and scale restated; the fraction is kept (fields are private).
fn restated(exact: &ExactMoney, currency: &str, scale: u8) -> ExactMoney {
    ExactMoney::new(exact.numerator(), exact.denominator(), currency, scale).unwrap()
}

fn line_id(n: u128) -> Uuid {
    Uuid::from_u128(100 + n)
}
fn day() -> Date {
    Date::from_calendar_date(2026, Month::October, 22).unwrap()
}
fn sel(item: Uuid, dim: Option<&str>) -> SelectedItem {
    SelectedItem {
        item_id: item,
        quantity: "1".into(),
        selected_dim_value: dim.map(str::to_owned),
    }
}
fn binding(item: Uuid, sku: Uuid, kind: &ChargeKind) -> AcceptedBinding {
    AcceptedBinding {
        item_id: item,
        price_book_entry_id: Uuid::from_u128(20),
        dimension_key: None,
        dimension_value: None,
        sku_id: sku,
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
        kind: kind.clone(),
        recurring_period: (*kind == ChargeKind::Recurring)
            .then_some(bss_pricing_sdk::terms::BillingCycle::Month),
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
fn cell(item: Uuid, b: AcceptedBinding) -> ResolvedCell {
    ResolvedCell {
        selection: BindingSelection {
            item_id: item,
            dimension_value: None,
        },
        binding: Some(b),
    }
}
fn sku(id: Uuid) -> Sku {
    let t = OffsetDateTime::UNIX_EPOCH;
    Sku {
        id,
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
/// A composed catalog answer: `ITEM` bound to the recurring `SKU`, `ITEM_B` to a one-time SKU.
fn catalog() -> CatalogAnswer {
    CatalogAnswer {
        current: Ok(RevisionRef {
            plan_id: PLAN,
            revision_id: REV,
            revision_no: 1,
        }),
        resolved: Ok(Arc::new(ResolvedBindings {
            plan_id: PLAN,
            revision_id: REV,
            cells: vec![
                cell(ITEM_B, binding(ITEM_B, ONE_TIME_SKU, &ChargeKind::OneTime)),
                cell(ITEM, binding(ITEM, SKU, &ChargeKind::Recurring)),
            ],
        })),
        skus: BTreeMap::from([
            (SKU, PortAnswer::Answered(sku(SKU))),
            (ONE_TIME_SKU, PortAnswer::Answered(sku(ONE_TIME_SKU))),
        ]),
    }
}
fn outage_catalog() -> CatalogAnswer {
    CatalogAnswer {
        current: Err(ReadFailure::Unavailable(Unavailable::Outage)),
        resolved: Err(ReadFailure::Unavailable(Unavailable::Outage)),
        skus: BTreeMap::new(),
    }
}
fn basket_line(n: u128, items: Vec<SelectedItem>, contract: Option<Uuid>) -> BasketLine {
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
        contract_ref: contract,
    }
}
fn policy() -> DatePolicySnapshot {
    let row = PolicyRow {
        policy_id: PLATFORM_DEFAULT_POLICY_ID,
        resource_tenant_id: None,
        service_activation_required: false,
        acceptance_due_required: false,
        revision: 1,
    };
    DatePolicySnapshot::effective(RESOURCE, &[row]).unwrap()
}
fn basket(operation: Operation, lines: Vec<BasketLine>) -> Basket {
    let supplied = lines.iter().map(|l| (l.line_id, SuppliedDates::default()));
    let dates = DateBasis::resolve(RESOURCE, policy(), day(), supplied);
    let terms = lines
        .iter()
        .map(|l| {
            let t = LineTerms {
                intent: LineTermIntent::Stated(TermIntentInput::Fixed { months: 12 }),
                cycle: Some(EvaluationCycle::Month),
            };
            (l.line_id, t)
        })
        .collect();
    Basket {
        assessed_at: OffsetDateTime::UNIX_EPOCH,
        target: match operation {
            Operation::Preview => GateTarget::Preview,
            Operation::Submit => GateTarget::Submit { order_id: ORDER },
            Operation::Amendment => GateTarget::Amendment { order_id: ORDER },
        },
        assessment_id: ASSESSMENT,
        axes: TenantAxes {
            resource: RESOURCE,
            payer: PAYER,
            seller: SELLER,
        },
        contract_id: None,
        lines,
        terms,
        resolve_date: day(),
        dates,
    }
}
fn two_lines() -> Vec<BasketLine> {
    vec![
        basket_line(1, vec![sel(ITEM, None), sel(ITEM_B, None)], None),
        basket_line(2, vec![sel(ITEM, None)], None),
    ]
}
fn profile(seller: bool, resource: bool) -> PayerProfile {
    PayerProfile {
        axes_valid: true,
        currency: "USD".into(),
        region: "US".into(),
        in_relationship_with_seller: seller,
        in_relationship_with_resource: resource,
        profile_version: "p1".into(),
    }
}
fn terms(line: Uuid) -> ResolvedTerms {
    ResolvedTerms {
        line_id: line,
        billing_terms: BillingTermsRef {
            digest: "terms".into(),
        },
        term: EvaluationTerm::FixedPeriods { count: 12 },
        earliest_start: None,
        provenance: TermsProvenance::ExplicitOrder,
        snapshot: Some(BillingTermsSnapshot {
            schema_version: 1,
            cycle: EvaluationCycle::Month,
            anchor: TermsAnchor::Calendar,
            anchor_at: OffsetDateTime::UNIX_EPOCH,
            timezone: "UTC".into(),
            source: TermsSnapshotSource::ExplicitOrder,
        }),
    }
}
fn overlap_key(line: Uuid, key: &str) -> OverlapKey {
    OverlapKey {
        item_key: None,
        line_id: line,
        catalog_subscription_product_key: format!("product-{key}"),
        overlap_scope_key: key.into(),
    }
}
fn occupancy(key: &str) -> (OccupancyQuery, OccupancyObservation) {
    (
        OccupancyQuery {
            own_context: None,
            payer_tenant_id: PAYER,
            resource_tenant_id: Some(RESOURCE),
            overlap_scope_key: key.into(),
        },
        Occupancy {
            active_count: 0,
            count_scope: bss_orders::domain::gate::answers::OccupancyCountScope::PerPayer,
            max_concurrent_active: MaxConcurrentActive::Limit(5),
            provenance: LimitProvenance::Catalog,
        }
        .into(),
    )
}
fn passing_assessment(lines: &[BasketLine]) -> AssessmentAnswer {
    let passed = |p: PredicateId| ProducerResult {
        producer: "pricing".into(),
        profile: "assess/1".into(),
        predicate: p.token().into(),
        verdict: Verdict::Passed,
        reason: None,
        detail: None,
    };
    let mut results = Vec::new();
    for l in lines {
        for p in [
            PredicateId::CatalogRevisionAvailable,
            PredicateId::CatalogEligibilityRules,
            PredicateId::MarketConsistency,
        ] {
            results.push(ProducerResultWithKey {
                line_id: l.line_id,
                item_id: None,
                selection: CatalogSelection::NotSelected,
                result: passed(p),
            });
        }
        for s in &l.catalog.items {
            results.push(ProducerResultWithKey {
                line_id: l.line_id,
                item_id: Some(s.item_id),
                selection: s
                    .selected_dim_value
                    .clone()
                    .map_or(CatalogSelection::SelectedNull, CatalogSelection::Selected),
                result: passed(PredicateId::CatalogDimensionMarket),
            });
        }
    }
    AssessmentAnswer {
        profile: bss_orders::domain::gate::orchestrate::ASSESSMENT_PROFILE.into(),
        observed_at: OffsetDateTime::UNIX_EPOCH,
        results,
    }
}
fn amount(n: &str) -> Amount {
    Amount {
        exact: ExactMoney::new(n, "1", "USD", 2).unwrap(),
        display: format!("{n}.00"),
    }
}
fn evaluation(lines: &[Uuid]) -> OrderEvaluation {
    let totals = |n: &str| Totals {
        gross: amount(n),
        net: amount(n),
        discount: Availability::Unavailable,
    };
    OrderEvaluation {
        assessment_id: ASSESSMENT,
        estimate: true,
        currency: "USD".into(),
        currency_scale: 2,
        lines: lines
            .iter()
            .map(|l| LineEvaluation {
                line_id: *l,
                totals: totals("10"),
                usage_flagged: false,
                committed_components: None,
                declaration: None,
                usage_declaration: None,
            })
            .collect(),
        order_totals: totals("20"),
        order_committed_components: None,
        order_tcv_carrier: None,
        order_declaration: None,
        order_usage_declaration: None,
        tcv: None,
        promotion_status: Availability::Unavailable,
        request_digest: "digest".into(),
    }
}
/// Every port answered for `b`, the payer relationship confirmed.
fn answered(b: &Basket) -> Answers {
    let ids: Vec<Uuid> = b.lines.iter().map(|l| l.line_id).collect();
    Answers {
        catalog: ids.iter().map(|l| (*l, catalog())).collect(),
        identity: PortAnswer::Answered(profile(true, true)),
        contracts: Some(PortAnswer::Answered(BTreeMap::new())),
        overlap_keys: Some(PortAnswer::Answered(
            ids.iter()
                .map(|l| overlap_key(*l, &format!("k{}", l.as_u128())))
                .collect(),
        )),
        occupancy: Some(PortAnswer::Answered(
            ids.iter()
                .map(|l| occupancy(&format!("k{}", l.as_u128())))
                .collect(),
        )),
        inflight_same_tuple: std::collections::BTreeSet::new(),
        inflight_other_orders: BTreeMap::new(),
        billing_terms: Some(PortAnswer::Answered(
            ids.iter().map(|l| terms(*l)).collect(),
        )),
        acceptance_policy: PortAnswer::Answered(Some(AcceptancePolicy {
            version: "v1".into(),
            hold_seconds: 600,
        })),
        assessment: PortAnswer::Answered(passing_assessment(&b.lines)),
        evaluation: Some(PortAnswer::Answered(evaluation(&ids))),
    }
}

// ---- plan ----

#[test]
fn plan_asks_contracts_only_when_referenced() {
    let b = basket(Operation::Submit, two_lines());
    assert!(plan(&b).contracts.is_empty());

    let lines = vec![
        basket_line(1, vec![sel(ITEM, None)], Some(CONTRACT_B)),
        basket_line(2, vec![sel(ITEM, None)], Some(CONTRACT)),
        basket_line(3, vec![sel(ITEM, None)], Some(CONTRACT_B)),
        basket_line(4, vec![sel(ITEM, None)], None),
    ];
    let p = plan(&basket(Operation::Submit, lines));
    // One query per distinct contract, in contract-id order, for the basket's payer and seller.
    assert_eq!(
        p.contracts,
        vec![
            ContractQuery {
                contract_id: CONTRACT,
                payer_tenant_id: PAYER,
                seller_tenant_id: SELLER,
            },
            ContractQuery {
                contract_id: CONTRACT_B,
                payer_tenant_id: PAYER,
                seller_tenant_id: SELLER,
            },
        ]
    );
}

#[test]
fn an_order_contract_is_asked_once_at_order_scope() {
    let mut b = basket(Operation::Submit, two_lines());
    b.contract_id = Some(CONTRACT);
    let p = plan(&b);
    assert_eq!(
        p.contracts,
        vec![ContractQuery {
            contract_id: CONTRACT,
            payer_tenant_id: PAYER,
            seller_tenant_id: SELLER,
        }]
    );
    // Repeated order/line references are queried once; distinct line contracts remain.
    b.lines[0].contract_ref = Some(CONTRACT);
    b.lines[1].contract_ref = Some(CONTRACT_B);
    assert_eq!(
        plan(&b)
            .contracts
            .iter()
            .map(|q| q.contract_id)
            .collect::<Vec<_>>(),
        vec![CONTRACT, CONTRACT_B]
    );
    b.lines[1].contract_ref = None;
    let input = assemble(&b, answered(&b));
    assert_eq!(input.contract_id, Some(CONTRACT));
    // The order's contract still reaches the terms queries of lines without their own.
    assert!(p.terms.iter().all(|t| t.contract_id == Some(CONTRACT)));
}

#[test]
fn plan_asks_identity_catalog_terms_and_policy_from_the_basket() {
    let mut lines = two_lines();
    lines[1].contract_ref = Some(CONTRACT_B);
    let mut b = basket(Operation::Submit, lines);
    b.terms.remove(&line_id(2));
    let p = plan(&b);
    assert_eq!(
        p.identity,
        PayerProfileQuery {
            payer_tenant_id: PAYER,
            seller_tenant_id: SELLER,
            resource_tenant_id: RESOURCE,
        }
    );
    // One catalog request per line, in basket order (M2-6: one `read_lines` call).
    let catalog: Vec<Uuid> = p.catalog.iter().map(|c| c.line_id).collect();
    assert_eq!(catalog, vec![line_id(1), line_id(2)]);
    // One terms query per line; a line without stated terms leaves them to the seller's policy.
    assert_eq!(
        p.terms,
        vec![
            BillingTermsQuery {
                line_id: line_id(1),
                payer_tenant_id: PAYER,
                seller_tenant_id: SELLER,
                plan_revision_id: REV,
                intent: TermIntentInput::Fixed { months: 12 },
                contract_id: None,
            },
            BillingTermsQuery {
                line_id: line_id(2),
                payer_tenant_id: PAYER,
                seller_tenant_id: SELLER,
                plan_revision_id: REV,
                intent: TermIntentInput::SellerPolicy,
                contract_id: Some(CONTRACT_B),
            },
        ]
    );
    assert_eq!(
        p.policy,
        AcceptancePolicyQuery {
            seller_tenant_id: SELLER
        }
    );
}

#[test]
fn only_preview_asks_for_tax() {
    assert!(plan(&basket(Operation::Preview, two_lines())).tax);
    assert!(!plan(&basket(Operation::Submit, two_lines())).tax);
    assert!(!plan(&basket(Operation::Amendment, two_lines())).tax);
}

// ---- identity gate (D-215, D-228) ----

#[test]
fn the_identity_gate_follows_both_relationship_flags() {
    let gate = |s, r| gate_after_identity(&PortAnswer::Answered(profile(s, r)));
    assert_eq!(gate(true, true), IdentityGate::Confirmed);
    assert_eq!(gate(false, true), IdentityGate::Unconfirmed);
    assert_eq!(gate(true, false), IdentityGate::Unconfirmed);
    assert_eq!(gate(false, false), IdentityGate::Unconfirmed);
    for kind in [
        Unavailable::Outage,
        Unavailable::Denied,
        Unavailable::Deadline,
        Unavailable::Malformed,
        Unavailable::Missing,
    ] {
        assert_eq!(
            gate_after_identity(&PortAnswer::Unavailable(kind)),
            IdentityGate::Unavailable
        );
    }
    // Invalid axes alone do not withhold: the assessment fails predicate 1 on them (D-228).
    let mut p = profile(true, true);
    p.axes_valid = false;
    assert_eq!(
        gate_after_identity(&PortAnswer::Answered(p)),
        IdentityGate::Confirmed
    );
}

#[test]
fn the_market_is_a_payer_fact_disclosed_only_once_confirmed() {
    assert_eq!(
        payer_market(&PortAnswer::Answered(profile(true, true))),
        Some(Market {
            currency: "USD".into(),
            region: "US".into()
        })
    );
    assert_eq!(
        payer_market(&PortAnswer::Answered(profile(true, false))),
        None
    );
    assert_eq!(
        payer_market(&PortAnswer::Unavailable(Unavailable::Outage)),
        None
    );
}

// ---- catalog-derived queries ----

#[test]
fn overlap_lines_name_each_composed_lines_paid_recurring_sku() {
    let mut lines = two_lines();
    lines.push(basket_line(3, vec![sel(ITEM_B, None)], None));
    lines.push(basket_line(4, vec![sel(ITEM, None)], None));
    let b = basket(Operation::Submit, lines);
    let mut answers: BTreeMap<Uuid, CatalogAnswer> =
        b.lines.iter().map(|l| (l.line_id, catalog())).collect();
    answers.insert(line_id(4), outage_catalog());
    let mut bindings = line_bindings(&b, &answers);
    // A usage-only line also has no overlap query; unselected recurring diagnostics do not count.
    for binding in bindings.get_mut(&line_id(3)).unwrap() {
        binding.kind = ChargeKind::Usage;
    }
    // Line 4 has no composed pin: no bindings, so nothing is asked for it.
    assert_eq!(
        bindings.keys().copied().collect::<Vec<_>>(),
        vec![line_id(1), line_id(2), line_id(3)]
    );
    assert_eq!(bindings[&line_id(1)].len(), 2);
    // Line 3 binds only a usage charge: no paid recurring SKU, so no key is asked.
    assert_eq!(
        overlap_key_lines(&b, &bindings),
        vec![
            OverlapKeyLine {
                item_id: ITEM,
                dimension_value: None,
                sku_version: 3,
                line_id: line_id(1),
                plan_revision_id: REV,
                paid_recurring_sku: SKU,
            },
            OverlapKeyLine {
                item_id: ITEM,
                dimension_value: None,
                sku_version: 3,
                line_id: line_id(2),
                plan_revision_id: REV,
                paid_recurring_sku: SKU,
            },
        ]
    );
}

#[test]
fn all_selected_recurring_bindings_are_requested() {
    let mut b = basket(Operation::Submit, two_lines());
    let answers = b.lines.iter().map(|l| (l.line_id, catalog())).collect();
    let mut bindings = line_bindings(&b, &answers);
    for binding in bindings.get_mut(&line_id(1)).unwrap() {
        binding.kind = ChargeKind::Recurring;
    }
    b.lines[0].catalog.items[0].selected_dim_value = Some("requested-fallback".into());
    let requested = overlap_key_lines(&b, &bindings);
    assert_eq!(
        requested[0].dimension_value.as_deref(),
        Some("requested-fallback")
    );
    assert_eq!(bindings[&line_id(1)][0].dimension_value, None);

    assert_eq!(
        requested.iter().filter(|q| q.line_id == line_id(1)).count(),
        2
    );
}

#[test]
fn occupancy_is_asked_once_per_overlap_scope_key() {
    let axes = TenantAxes {
        resource: RESOURCE,
        payer: PAYER,
        seller: SELLER,
    };
    let keys = vec![
        overlap_key(line_id(1), "k-b"),
        overlap_key(line_id(2), "k-a"),
        overlap_key(line_id(3), "k-b"),
    ];
    let q = occupancy_queries(&keys, &axes);
    assert_eq!(
        q,
        vec![
            OccupancyQuery {
                own_context: None,
                payer_tenant_id: PAYER,
                resource_tenant_id: Some(RESOURCE),
                overlap_scope_key: "k-a".into(),
            },
            OccupancyQuery {
                own_context: None,
                payer_tenant_id: PAYER,
                resource_tenant_id: Some(RESOURCE),
                overlap_scope_key: "k-b".into(),
            },
        ]
    );
    assert!(occupancy_queries(&[], &axes).is_empty());
}

#[test]
fn a_line_with_more_than_one_key_has_no_resolved_key() {
    let keys = vec![
        overlap_key(line_id(1), "k-a"),
        overlap_key(line_id(2), "k-a"),
        overlap_key(line_id(2), "k-b"),
        // The same key twice is still one key.
        overlap_key(line_id(3), "k-c"),
        overlap_key(line_id(3), "k-c"),
    ];
    let resolved = line_keys(&keys);
    assert_eq!(resolved[&line_id(1)].overlap_scope_key, "k-a");
    assert!(!resolved.contains_key(&line_id(2)));
    assert_eq!(resolved[&line_id(3)].overlap_scope_key, "k-c");
    assert_eq!(resolved.len(), 2);
}

#[test]
fn the_assessment_query_carries_market_terms_and_cycle_when_known() {
    let b = basket(Operation::Submit, two_lines());
    let market = Market {
        currency: "USD".into(),
        region: "US".into(),
    };
    let t = vec![terms(line_id(2))];
    let q = assessment_query(&b, Some(&market), Some(&t));
    assert_eq!(q.assessment_id, ASSESSMENT);
    assert_eq!(q.seller_tenant_id, SELLER);
    assert_eq!(q.resolve_date, day());
    assert_eq!(q.lines.len(), 2);
    assert_eq!(q.lines[0].line_id, line_id(1));
    assert_eq!(q.lines[0].selections, b.lines[0].catalog.items);
    assert_eq!(q.lines[0].market.as_ref(), Some(&market));
    assert_eq!(q.lines[0].term, None);
    assert_eq!(
        q.lines[1].term,
        Some(EvaluationTerm::FixedPeriods { count: 12 })
    );
    assert_eq!(q.lines[1].cycle, Some(EvaluationCycle::Month));
    // Without a confirmed payer the market is never sent.
    let q = assessment_query(&b, None, None);
    assert!(
        q.lines
            .iter()
            .all(|l| l.market.is_none() && l.term.is_none())
    );
}

// ---- evaluation request ----

fn composed(b: &Basket) -> BTreeMap<Uuid, Vec<AcceptedBinding>> {
    let answers = b.lines.iter().map(|l| (l.line_id, catalog())).collect();
    line_bindings(b, &answers)
}
fn market() -> Market {
    Market {
        currency: "USD".into(),
        region: "US".into(),
    }
}

#[test]
fn no_evaluation_is_requested_without_terms() {
    let b = basket(Operation::Preview, two_lines());
    let bindings = composed(&b);
    assert!(
        evaluation_request(&b, &bindings, None, Some(&market()), None)
            .unwrap()
            .is_none()
    );
    // One line without terms withholds the whole request.
    let partial = vec![terms(line_id(1))];
    assert!(
        evaluation_request(&b, &bindings, Some(&partial), Some(&market()), None)
            .unwrap()
            .is_none()
    );
}

#[test]
fn no_evaluation_is_requested_without_bindings_market_or_cycle() {
    let b = basket(Operation::Preview, two_lines());
    let all = vec![terms(line_id(1)), terms(line_id(2))];
    let mut bindings = composed(&b);
    assert!(
        evaluation_request(&b, &bindings, Some(&all), None, None)
            .unwrap()
            .is_none()
    );
    let mut no_cycle = b.clone();
    no_cycle.terms.get_mut(&line_id(2)).unwrap().cycle = None;
    assert!(
        evaluation_request(&no_cycle, &bindings, Some(&all), Some(&market()), None)
            .unwrap()
            .is_some()
    );
    for binding in bindings.get_mut(&line_id(2)).unwrap() {
        binding.recurring_period = None;
    }
    // With no authored cycle and no recurring period, line 2 has no cycle.
    assert!(
        evaluation_request(&no_cycle, &bindings, Some(&all), Some(&market()), None)
            .unwrap()
            .is_none()
    );
    bindings.remove(&line_id(1));
    assert!(
        evaluation_request(&b, &bindings, Some(&all), Some(&market()), None)
            .unwrap()
            .is_none()
    );
}

/// A binding the schema-2 encoder refuses is a local defect on its line, not a missing
/// prerequisite (D-220 item 1: an encoder refusal is always `Unsupported`).
#[test]
fn evaluation_request_reports_an_unencodable_binding() {
    let b = basket(Operation::Preview, two_lines());
    let all = vec![terms(line_id(1)), terms(line_id(2))];
    let mut bindings = composed(&b);
    // `scalars::date` writes four-digit years only (`orders-sdk/tests/commercial_codec.rs`).
    bindings.get_mut(&line_id(2)).unwrap()[0].price.ends_on =
        Some(Date::from_calendar_date(-1, Month::January, 1).unwrap());
    let err = evaluation_request(&b, &bindings, Some(&all), Some(&market()), None).unwrap_err();
    assert_eq!(
        err,
        CommercialError::Unsupported {
            line_id: line_id(2),
            what: "native binding encoding",
        }
    );
}

#[test]
fn the_evaluation_request_prices_exactly_the_accepted_bindings() {
    let mut b = basket(Operation::Preview, two_lines());
    b.contract_id = Some(CONTRACT);
    let all = vec![terms(line_id(2)), terms(line_id(1))];
    let r = evaluation_request(&b, &composed(&b), Some(&all), Some(&market()), None)
        .unwrap()
        .unwrap();
    assert_eq!(r.assessment_id, ASSESSMENT);
    assert_eq!(r.resolve_date, day());
    assert_eq!(r.market, market());
    assert_eq!(r.contract_id, Some(CONTRACT));
    let ids: Vec<Uuid> = r.lines.iter().map(|l| l.line_id).collect();
    assert_eq!(ids, vec![line_id(1), line_id(2)]);
    let l1 = &r.lines[0];
    assert_eq!(l1.plan_revision_id, REV);
    assert_eq!(l1.term, EvaluationTerm::FixedPeriods { count: 12 });
    assert_eq!(l1.cycle, EvaluationCycle::Month);
    let items: Vec<Uuid> = l1.items.iter().map(|i| i.item_id).collect();
    assert_eq!(items, vec![ITEM, ITEM_B]);
    let chain = &l1.items[0].chains[0];
    assert_eq!(chain.dim_value, None);
    assert_eq!(
        chain.binding,
        EvaluationBinding {
            native_binding: Some(
                bss_orders_sdk::commercial::encode_accepted_binding(&composed(&b)[&line_id(1)][0])
                    .unwrap()
            ),
            item_id: ITEM,
            price_book_entry_id: Uuid::from_u128(20),
            price_id: Uuid::from_u128(21),
            money_digest: "07".repeat(32),
            currency: "USD".into(),
            currency_scale: 2,
            sku_id: SKU,
            sku_version: 3,
            dimension_key: None,
            dimension_value: None,
            kind: EvaluationChargeKind::Recurring,
            recurring_period: Some(EvaluationCycle::Month),
            via_default: false,
        }
    );
    assert_eq!(
        l1.items[1].chains[0].binding.kind,
        EvaluationChargeKind::OneTime
    );
    assert_eq!(l1.items[0].quantity, "1");
}

#[test]
fn tax_is_asked_on_preview_over_the_evaluated_line_amounts() {
    let b = basket(Operation::Preview, two_lines());
    let ev = evaluation(&[line_id(1), line_id(2)]);
    let q = tax_query(&b, &ev).unwrap();
    assert_eq!(q.payer_tenant_id, PAYER);
    assert_eq!(q.seller_tenant_id, SELLER);
    assert_eq!(
        q.lines,
        [1, 2]
            .map(|n| LineAmount {
                line_id: line_id(n),
                amount: amount("10"),
            })
            .to_vec()
    );
    assert!(tax_query(&basket(Operation::Submit, two_lines()), &ev).is_none());
}

// ---- assemble ----

#[test]
fn assemble_passes_every_answer_through() {
    let b = basket(Operation::Submit, two_lines());
    let a = answered(&b);
    let mut with_inflight = a.clone();
    with_inflight.inflight_same_tuple.insert("k101".to_owned());
    with_inflight.inflight_other_orders = BTreeMap::from([("k101".to_owned(), 2)]);
    let input = assemble(&b, with_inflight);
    assert_eq!(input.operation, Operation::Submit);
    assert_eq!(input.resolve_date, day());
    assert_eq!(input.lines, b.lines);
    assert_eq!(input.dates, b.dates);
    assert_eq!(input.identity, a.identity);
    assert_eq!(Some(input.contracts), a.contracts);
    assert_eq!(Some(input.overlap_keys), a.overlap_keys);
    assert_eq!(Some(input.occupancy), a.occupancy);
    assert_eq!(Some(input.billing_terms), a.billing_terms);
    assert_eq!(input.acceptance_policy, a.acceptance_policy);
    assert_eq!(input.assessment, a.assessment);
    assert_eq!(Some(input.evaluation), a.evaluation);
    assert_eq!(input.inflight_other_orders["k101"], 2);
    assert_eq!(
        input.inflight_same_tuple,
        ["k101".to_owned()].into_iter().collect()
    );
    assert_eq!(input.catalog.len(), 2);
}

#[test]
fn a_port_not_asked_is_missing_and_unconfirmed_payer_inputs_are_resolved_by_withholding() {
    let b = basket(Operation::Submit, two_lines());
    let mut a = answered(&b);
    a.identity = PortAnswer::Answered(profile(true, false));
    // The shell never calls the payer-dependent ports nor billing terms (their query carries
    // payer facts) on an unconfirmed relationship (D-215 item 2).
    a.contracts = None;
    a.overlap_keys = None;
    a.occupancy = None;
    a.billing_terms = None;
    a.evaluation = None;
    let input = assemble(&b, a);
    assert!(matches!(
        input.contracts,
        PortAnswer::Unavailable(Unavailable::Missing)
    ));
    assert!(matches!(
        input.overlap_keys,
        PortAnswer::Unavailable(Unavailable::Missing)
    ));
    assert!(matches!(
        input.occupancy,
        PortAnswer::Unavailable(Unavailable::Missing)
    ));
    assert_eq!(
        input.evaluation,
        PortAnswer::Skipped(bss_orders_sdk::catalog::Reason::PayerRelationshipUnconfirmed)
    );
    assert!(matches!(
        input.billing_terms,
        PortAnswer::Unavailable(Unavailable::Missing)
    ));
    let out = assess(&input).unwrap();
    // D-228 item 6: the gate resolves the withheld inputs, the unasked evaluation included
    // (never `Precluded`: predicate 1 is inside the gate, not an earlier guard); with the
    // primary a definitive local refusal every gate input is `Resolved` (D-230).
    for p in [
        InputPort::Identity,
        InputPort::PricingRevision,
        InputPort::CatalogPredicates,
        InputPort::ContractResolution,
        InputPort::OverlapKey,
        InputPort::Occupancy,
        InputPort::PinComposition,
        InputPort::Evaluation,
    ] {
        assert_eq!(out.inputs.get(p), Some(InputState::Resolved), "{p:?}");
    }
    // Unasked terms are withheld, not a port outage (D-228 item 6): the pin rows carry
    // predicate 1's reason and no failure carries the 503 pin reason.
    let unconfirmed = bss_orders_sdk::catalog::Reason::PayerRelationshipUnconfirmed;
    for l in [line_id(1), line_id(2)] {
        let pin = out
            .vector
            .outcomes()
            .iter()
            .find(|o| o.key.predicate == PredicateId::PinComposition && o.key.line_id == Some(l))
            .expect("pin row");
        assert_eq!(pin.reason, Some(unconfirmed), "{l}");
    }
    assert!(
        out.failures
            .iter()
            .all(|f| f.reason != bss_orders_sdk::catalog::Reason::CatalogPinCompositionUnavailable),
        "{:#?}",
        out.failures
    );
    assert_eq!(out.primary.and_then(|o| o.reason), Some(unconfirmed));
}

/// D-242 (C6 × B3): a refused purchase shape (items stating different quantities) is never
/// composed, so the orchestrator never asks a key for it; the full key answer then holds no entry
/// `assess` calls foreign, and the other keyed line's rows 7/9 report the in-flight read, never
/// `overlap-key-answer-rejected`.
#[test]
fn a_mixed_quantity_line_is_never_asked_a_key_and_leaves_other_lines_read() {
    let mixed = |item: Uuid, qty: &str| SelectedItem {
        quantity: qty.into(),
        ..sel(item, None)
    };
    let (l1, l2) = (line_id(1), line_id(2));
    let b = basket(
        Operation::Submit,
        vec![
            basket_line(1, vec![sel(ITEM, None)], None),
            basket_line(2, vec![mixed(ITEM, "1"), mixed(ITEM_B, "2")], None),
        ],
    );
    let mut a = answered(&b);
    // What the shell does: compose, then ask one key per requested line.
    let bindings = line_bindings(&b, &a.catalog);
    assert_eq!(bindings.keys().copied().collect::<Vec<_>>(), vec![l1]);
    let asked: std::collections::BTreeSet<Uuid> = overlap_key_lines(&b, &bindings)
        .iter()
        .map(|q| q.line_id)
        .collect();
    assert_eq!(asked, [l1].into_iter().collect());
    let k1 = format!("k{}", l1.as_u128());
    a.overlap_keys = Some(PortAnswer::Answered(
        asked
            .iter()
            .map(|l| overlap_key(*l, &format!("k{}", l.as_u128())))
            .collect(),
    ));
    a.occupancy = Some(PortAnswer::Answered(vec![occupancy(&k1)]));
    // l1's key is held by an in-flight order on the same tuple: the read ran, so row 9 fails.
    a.inflight_same_tuple = [k1].into_iter().collect();
    let out = assess(&assemble(&b, a)).unwrap();
    let row = |p: PredicateId, l: Uuid| {
        out.vector
            .outcomes()
            .iter()
            .find(|o| o.key.predicate == p && o.key.line_id == Some(l))
            .unwrap_or_else(|| panic!("{p:?} row of {l}"))
    };
    let in_flight = row(PredicateId::InFlightPerKey, l1);
    assert_eq!(
        (in_flight.verdict, in_flight.reason),
        (Verdict::Failed, Some(Reason::OrderInFlightForKey)),
        "{in_flight:#?}"
    );
    let unique = row(PredicateId::OverlapUniqueness, l1);
    assert_eq!(unique.verdict, Verdict::Passed, "{unique:#?}");
    let refused = row(PredicateId::ReferenceResolution, l2);
    assert_eq!(
        (refused.verdict, refused.reason),
        (Verdict::Failed, Some(Reason::ReferenceUnresolvable))
    );
}

#[test]
fn the_assessment_is_deterministic_across_answer_order() {
    let b = basket(Operation::Submit, two_lines());
    let a = answered(&b);
    let expected = assess(&assemble(&b, a.clone())).unwrap();
    assert!(expected.primary.is_none(), "{:?}", expected.primary);

    let reverse = |v: &mut Vec<_>| v.reverse();
    let mut shuffled = a;
    if let Some(PortAnswer::Answered(k)) = shuffled.overlap_keys.as_mut() {
        reverse(k);
    }
    if let Some(PortAnswer::Answered(o)) = shuffled.occupancy.as_mut() {
        o.reverse();
    }
    if let Some(PortAnswer::Answered(t)) = shuffled.billing_terms.as_mut() {
        t.reverse();
    }
    if let PortAnswer::Answered(r) = &mut shuffled.assessment {
        r.results.reverse();
    }
    let mut reordered = b;
    reordered.lines.reverse();
    assert_eq!(assess(&assemble(&reordered, shuffled)).unwrap(), expected);
}

#[test]
fn failures_and_the_primary_are_deterministic_across_answer_order() {
    let mut b = basket(Operation::Submit, two_lines());
    // Two failing lines: every quantity of both lines is below the floor, and line 2's terms
    // are missing (its pin is unevaluable).
    for line in &mut b.lines {
        for item in &mut line.catalog.items {
            item.quantity = "0".into();
        }
    }
    let mut a = answered(&b);
    if let Some(PortAnswer::Answered(t)) = a.billing_terms.as_mut() {
        t.retain(|t| t.line_id != line_id(2));
    }
    let expected = assess(&assemble(&b, a.clone())).unwrap();
    let failed_lines: std::collections::BTreeSet<_> =
        expected.failures.iter().map(|f| f.line_id).collect();
    assert!(
        failed_lines.contains(&Some(line_id(1))) && failed_lines.contains(&Some(line_id(2))),
        "{:?}",
        expected.failures
    );
    assert!(expected.primary.is_some());

    let mut shuffled = a;
    if let Some(PortAnswer::Answered(k)) = shuffled.overlap_keys.as_mut() {
        k.reverse();
    }
    if let Some(PortAnswer::Answered(o)) = shuffled.occupancy.as_mut() {
        o.reverse();
    }
    if let Some(PortAnswer::Answered(t)) = shuffled.billing_terms.as_mut() {
        t.reverse();
    }
    if let PortAnswer::Answered(r) = &mut shuffled.assessment {
        r.results.reverse();
    }
    let mut reordered = b;
    reordered.lines.reverse();
    let out = assess(&assemble(&reordered, shuffled)).unwrap();
    assert_eq!(out.failures, expected.failures);
    assert_eq!(out.primary, expected.primary);
    assert_eq!(out, expected);
}

// ---- D-227 boundary ----

#[test]
fn the_orchestrator_never_names_the_phase_b_sellability_check() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for file in [
        "src/domain/gate/orchestrate.rs",
        "src/infra/gate/orchestrate.rs",
    ] {
        let text = std::fs::read_to_string(root.join(file)).unwrap();
        assert!(!text.contains("SellabilityV1"), "{file}");
    }
}

#[test]
fn authored_terms_use_exact_invoice_periods_or_refuse_without_a_query() {
    let calendar = |years, months, days| AuthoredTerm::Calendar {
        years,
        months,
        days,
        microseconds: 0,
    };
    let cases = [
        (
            Some(calendar(1, 0, 0)),
            Some(BillingCycle::Month),
            Some(TermIntentInput::Fixed { months: 12 }),
        ),
        (
            Some(calendar(1, 0, 0)),
            Some(BillingCycle::Year),
            Some(TermIntentInput::Fixed { months: 12 }),
        ),
        (Some(calendar(1, 6, 0)), Some(BillingCycle::Year), None),
        (Some(calendar(1, 0, 10)), Some(BillingCycle::Month), None),
        (Some(AuthoredTerm::Periods { count: 3 }), None, None),
        (None, None, Some(TermIntentInput::SellerPolicy)),
        (
            Some(AuthoredTerm::Rolling),
            None,
            Some(TermIntentInput::Rolling),
        ),
    ];
    for (term, cycle, expected) in cases {
        let mut b = basket(Operation::Submit, vec![two_lines().remove(0)]);
        b.terms.insert(line_id(1), line_terms(term.as_ref(), cycle));
        let queries = plan(&b).terms;
        assert_eq!(
            queries.first().map(|q| q.intent),
            expected,
            "{term:?} {cycle:?}"
        );
        if expected.is_none() {
            let mut answers = answered(&b);
            answers.billing_terms = Some(PortAnswer::Answered(vec![]));
            answers.evaluation = None;
            let result = assess(&assemble(&b, answers)).unwrap();
            assert_eq!(
                result.primary.as_ref().unwrap().reason.unwrap().token(),
                "billing-terms-unsupported"
            );
            let row = result
                .vector
                .outcomes()
                .iter()
                .find(|r| r.key.predicate == PredicateId::PinComposition)
                .unwrap();
            assert_eq!(row.verdict, Verdict::Failed);
            assert_eq!(row.reason.unwrap().token(), "billing-terms-unsupported");
        }
    }
}

#[test]
fn pin_refuses_terms_and_recurring_cycles_that_disagree_with_evaluation() {
    for operation in [Operation::Preview, Operation::Submit] {
        for (snapshot_cycle, recurring_cycle) in [
            (
                EvaluationCycle::Year,
                bss_pricing_sdk::terms::BillingCycle::Month,
            ),
            (
                EvaluationCycle::Month,
                bss_pricing_sdk::terms::BillingCycle::Year,
            ),
        ] {
            let b = basket(operation, vec![two_lines().remove(0)]);
            let mut a = answered(&b);
            if let Some(PortAnswer::Answered(terms)) = &mut a.billing_terms {
                terms[0].snapshot = Some(BillingTermsSnapshot {
                    schema_version: 1,
                    cycle: snapshot_cycle,
                    anchor: TermsAnchor::Calendar,
                    anchor_at: OffsetDateTime::UNIX_EPOCH,
                    timezone: "UTC".into(),
                    source: TermsSnapshotSource::ExplicitOrder,
                });
            }
            let cat = a.catalog.get_mut(&line_id(1)).unwrap();
            for (index, cell) in Arc::make_mut(cat.resolved.as_mut().unwrap())
                .cells
                .iter_mut()
                .enumerate()
            {
                let binding = cell.binding.as_mut().unwrap();
                binding.kind = ChargeKind::Recurring;
                binding.recurring_period = Some(if index == 0 {
                    recurring_cycle
                } else {
                    bss_pricing_sdk::terms::BillingCycle::Month
                });
            }
            let out = assess(&assemble(&b, a)).unwrap();
            let row = out
                .vector
                .outcomes()
                .iter()
                .find(|r| r.key.predicate == PredicateId::PinComposition)
                .unwrap();
            assert_eq!(
                row.verdict,
                Verdict::Failed,
                "{operation:?} {snapshot_cycle:?} {recurring_cycle:?}"
            );
            assert_eq!(row.reason.unwrap().token(), "billing-terms-unsupported");
            if operation == Operation::Preview {
                use bss_orders::domain::gate::report::{
                    PreviewAnswers, PreviewLineFacts, preview_result,
                };
                let result = preview_result(
                    ASSESSMENT,
                    &out,
                    PreviewAnswers {
                        evaluation: None,
                        tax: None,
                    },
                    &[PreviewLineFacts {
                        line_id: line_id(1),
                        term_and_cycle_authored: true,
                        service_activation: Some(day()),
                    }],
                    day(),
                );
                assert!(result.tcv.is_none());
                assert!(result.tcv_withheld.is_some());
            }
        }
    }
}

/// R5 (OQ-C1 a, amends D-236 item 2): terms submit cannot freeze into a Pricing request (no
/// snapshot, another schema, a non-UTC zone, a seller-policy source at version 0) refuse the
/// pin-composition row `billing-terms-unsupported` on every operation, never a 500 at submit.
#[test]
fn a_terms_snapshot_pricing_cannot_express_refuses_billing_terms_unsupported() {
    for operation in [Operation::Preview, Operation::Submit] {
        let pin = |a: Answers, b: &Basket| {
            let out = assess(&assemble(b, a)).unwrap();
            let row = out
                .vector
                .outcomes()
                .iter()
                .find(|r| r.key.predicate == PredicateId::PinComposition)
                .unwrap()
                .clone();
            (row.verdict, row.reason)
        };
        let b = basket(operation, vec![two_lines().remove(0)]);
        assert_eq!(
            pin(answered(&b), &b),
            (Verdict::Passed, None),
            "{operation:?} control"
        );
        for case in 0..4 {
            let mut a = answered(&b);
            let Some(PortAnswer::Answered(terms)) = &mut a.billing_terms else {
                panic!("answered terms")
            };
            match case {
                0 => terms[0].snapshot = None,
                1 => terms[0].snapshot.as_mut().unwrap().timezone = "Europe/Berlin".into(),
                2 => terms[0].snapshot.as_mut().unwrap().schema_version = 2,
                _ => {
                    terms[0].snapshot.as_mut().unwrap().source =
                        TermsSnapshotSource::SellerPolicy {
                            id: Uuid::from_u128(9),
                            version: 0,
                        };
                }
            }
            let (verdict, reason) = pin(a, &b);
            assert_eq!(verdict, Verdict::Failed, "{operation:?} case {case}");
            assert_eq!(
                reason.map(bss_orders_sdk::catalog::Reason::token),
                Some("billing-terms-unsupported"),
                "{operation:?} case {case}"
            );
        }
    }
}

#[test]
fn skipped_evaluation_keeps_terms_or_catalog_source_reason() {
    for missing_terms in [true, false] {
        let b = basket(Operation::Submit, two_lines());
        let mut a = answered(&b);
        a.evaluation = None;
        let expected = if missing_terms {
            a.billing_terms = Some(PortAnswer::Unavailable(Unavailable::Outage));
            bss_orders_sdk::catalog::Reason::CatalogPinCompositionUnavailable
        } else {
            a.catalog.clear();
            bss_orders_sdk::catalog::Reason::CatalogPredicatesUnavailable
        };
        let assessment = assess(&assemble(&b, a)).unwrap();
        assert_eq!(
            assessment.inputs.get(InputPort::Evaluation),
            Some(InputState::Unresolvable(expected))
        );
    }
}

#[test]
fn evaluation_envelope_rejects_identity_coverage_and_money_currency_mismatches() {
    use bss_orders::domain::gate::orchestrate::verify_evaluation;
    let b = basket(Operation::Preview, two_lines());
    let all = vec![terms(line_id(1)), terms(line_id(2))];
    let request = evaluation_request(&b, &composed(&b), Some(&all), Some(&market()), None)
        .unwrap()
        .unwrap();
    let good = evaluation(&[line_id(2), line_id(1)]);
    assert_eq!(verify_evaluation(&request, &good), Ok(()));
    for case in 0..11 {
        let mut answer = good.clone();
        match case {
            0 => answer.assessment_id = Uuid::nil(),
            1 => {
                answer.lines.pop();
            }
            2 => answer.lines.push(answer.lines[0].clone()),
            3 => answer.lines[0].line_id = Uuid::nil(),
            4 => answer.currency = "EUR".into(),
            5 => answer.order_totals.net.exact = restated(&answer.order_totals.net.exact, "EUR", 2),
            6 => {
                answer.lines[0].totals.gross.exact =
                    restated(&answer.lines[0].totals.gross.exact, "EUR", 2);
            }
            7 => {
                answer.lines[0].totals.discount = Availability::Available(Amount {
                    exact: ExactMoney::new("1", "1", "EUR", 2).unwrap(),
                    display: "1".into(),
                });
            }
            8 => {
                let net = &answer.order_totals.net.exact;
                answer.order_totals.net.exact = restated(net, net.currency(), 3);
            }
            9 => {
                let mut extra = answer.lines[0].clone();
                extra.line_id = Uuid::nil();
                answer.lines.push(extra);
            }
            _ => {
                answer.tcv = Some(Tcv {
                    exact: ExactMoney::new("1", "1", "EUR", 2).unwrap(),
                    display: "1".into(),
                    posting_estimate: "1".into(),
                    basis: None,
                    evidence: None,
                });
            }
        }
        assert_eq!(
            verify_evaluation(&request, &answer),
            Err(Unavailable::Malformed),
            "case {case}"
        );
    }
}

#[test]
fn evaluation_scale_boundary_requires_a_coherent_envelope() {
    use bss_orders::domain::gate::orchestrate::verify_evaluation;
    let b = basket(Operation::Preview, two_lines());
    let all = vec![terms(line_id(1)), terms(line_id(2))];
    let base = evaluation_request(&b, &composed(&b), Some(&all), Some(&market()), None)
        .unwrap()
        .unwrap();
    for scale in [28, 29] {
        // The request's prices are at `scale` too, so only the 28 ceiling decides.
        let mut request = base.clone();
        for chain in request
            .lines
            .iter_mut()
            .flat_map(|l| &mut l.items)
            .flat_map(|i| &mut i.chains)
        {
            chain.binding.currency_scale = scale;
        }
        let mut answer = evaluation(&[line_id(1), line_id(2)]);
        answer.currency_scale = scale;
        for totals in std::iter::once(&mut answer.order_totals)
            .chain(answer.lines.iter_mut().map(|line| &mut line.totals))
        {
            totals.gross.exact = restated(&totals.gross.exact, "USD", scale);
            totals.net.exact = restated(&totals.net.exact, "USD", scale);
            totals.discount = Availability::Available(Amount {
                exact: ExactMoney::new("0", "1", "USD", scale).unwrap(),
                display: "0".into(),
            });
        }
        answer.tcv = Some(Tcv {
            exact: ExactMoney::new("1", "1", "USD", scale).unwrap(),
            display: "1".into(),
            posting_estimate: "1".into(),
            basis: None,
            evidence: None,
        });
        assert_eq!(
            verify_evaluation(&request, &answer),
            if scale == 28 {
                Ok(())
            } else {
                Err(Unavailable::Malformed)
            },
            "scale {scale}"
        );
    }
}

/// R7 / D-197: Rating answers at the scale of the prices it was asked to evaluate; a USD
/// evaluation at scale 4 over USD prices at scale 2 is a currency inconsistency.
#[test]
fn an_evaluation_at_another_scale_than_the_evaluated_prices_is_malformed() {
    use bss_orders::domain::gate::orchestrate::verify_evaluation;
    let b = basket(Operation::Preview, two_lines());
    let all = vec![terms(line_id(1)), terms(line_id(2))];
    let request = evaluation_request(&b, &composed(&b), Some(&all), Some(&market()), None)
        .unwrap()
        .unwrap();
    let mut answer = evaluation(&[line_id(1), line_id(2)]);
    answer.currency_scale = 4;
    for totals in std::iter::once(&mut answer.order_totals)
        .chain(answer.lines.iter_mut().map(|line| &mut line.totals))
    {
        totals.gross.exact = restated(&totals.gross.exact, "USD", 4);
        totals.net.exact = restated(&totals.net.exact, "USD", 4);
    }
    assert_eq!(
        verify_evaluation(&request, &answer),
        Err(Unavailable::Malformed)
    );
}

#[test]
fn the_shipped_development_profile_admits_a_valid_assessment() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../config/gate-dev-stubs.example.json")).unwrap();
    let b = basket(Operation::Preview, two_lines());
    let mut a = answered(&b);
    let PortAnswer::Answered(envelope) = &mut a.assessment else {
        panic!("answered fixture")
    };
    envelope.profile = fixture["assessment_profile"].as_str().unwrap().into();
    let assessment = assess(&assemble(&b, a)).unwrap();
    assert!(
        !assessment
            .failures
            .iter()
            .any(|f| f.reason == bss_orders_sdk::catalog::Reason::CatalogPredicatesUnavailable),
        "{:?}",
        assessment.failures
    );
}

#[test]
fn rating_request_preserves_native_models_and_resolved_context() {
    let mut b = basket(Operation::Preview, two_lines());
    b.terms.get_mut(&line_id(2)).unwrap().cycle = Some(EvaluationCycle::Year);
    let mut all = vec![terms(line_id(1)), terms(line_id(2))];
    all[0].earliest_start = Some(day().previous_day().unwrap().midnight().assume_utc());
    all[1].term = EvaluationTerm::Rolling;
    all[1].snapshot = Some(BillingTermsSnapshot {
        schema_version: 1,
        cycle: EvaluationCycle::Year,
        anchor: TermsAnchor::SubscriptionStart,
        anchor_at: day().midnight().assume_utc(),
        timezone: "UTC".into(),
        source: TermsSnapshotSource::SellerPolicy {
            id: Uuid::from_u128(998),
            version: 19,
        },
    });
    all[0].snapshot = Some(BillingTermsSnapshot {
        schema_version: 1,
        cycle: EvaluationCycle::Month,
        anchor: TermsAnchor::Calendar,
        anchor_at: day().midnight().assume_utc(),
        timezone: "UTC".into(),
        source: TermsSnapshotSource::ExplicitOrder,
    });
    let mut bound = composed(&b);
    bound.get_mut(&line_id(2)).unwrap()[0].recurring_period =
        Some(bss_pricing_sdk::terms::BillingCycle::Year);
    bound.get_mut(&line_id(1)).unwrap()[0].price.minimum_fee =
        Some(bss_money::parse_decimal("3.25").unwrap());
    let request = evaluation_request(&b, &bound, Some(&all), Some(&market()), None)
        .unwrap()
        .unwrap();
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(value["assessed_at"], "1970-01-01T00:00:00Z");
    assert_eq!(value["tenant_axes"]["resource"], RESOURCE.to_string());
    assert_eq!(value["lines"][0]["plan_id"], PLAN.to_string());
    assert_eq!(value["lines"][0]["start_at"], "2026-10-22T00:00:00Z");
    assert_eq!(
        value["lines"][0]["resolved_terms"]["billing_terms"]["digest"],
        "terms"
    );
    assert_eq!(
        value["lines"][0]["resolved_terms"]["snapshot"]["timezone"],
        "UTC"
    );
    assert_eq!(
        value["lines"][0]["resolved_terms"]["earliest_start"],
        "2026-10-21T00:00:00Z"
    );
    assert_eq!(value["lines"][1]["term"], "rolling");
    assert_eq!(value["lines"][1]["cycle"], "year");
    assert_eq!(
        value["lines"][1]["resolved_terms"]["snapshot"]["source"]["version"],
        19
    );
    let native = &value["lines"][0]["items"][0]["chains"][0]["binding"]["native_binding"];
    assert_eq!(native["price"]["minimum_fee"], "3.25");
    assert_eq!(native["price"]["model"]["amount"], "10");
    assert_eq!(native["invoice"]["template_digest"], "08".repeat(32));
}

/// Omitting native commercial inputs loses model-specific evaluation rules or policy evidence.
#[test]
fn rating_native_inputs_keep_each_model_and_usage_policy_without_evaluation() {
    use bss_pricing_sdk::terms::*;
    use serde_json::json;
    let d = |s| bss_money::parse_decimal(s).unwrap();
    let tiers = vec![
        Tier {
            up_to: Some(d("9.5")),
            rate: d("0.125"),
        },
        Tier {
            up_to: None,
            rate: d("0.1"),
        },
    ];
    let models = [
        (
            PriceModel::Flat { amount: d("17.25") },
            json!({"kind":"flat", "amount":"17.25"}),
        ),
        (
            PriceModel::PerUnit {
                unit_amount: d("0.125"),
            },
            json!({"kind":"per_unit", "unit_amount":"0.125"}),
        ),
        (
            PriceModel::Volume {
                tiers: tiers.clone(),
            },
            json!({"kind":"volume", "tiers":[{"up_to":"9.5", "rate":"0.125"},{"up_to":null,"rate":"0.1"}]}),
        ),
        (
            PriceModel::Graduated { tiers },
            json!({"kind":"graduated", "tiers":[{"up_to":"9.5", "rate":"0.125"},{"up_to":null,"rate":"0.1"}]}),
        ),
        (
            PriceModel::Package {
                package_size: d("10"),
                package_price: d("7.5"),
            },
            json!({"kind":"package", "package_size":"10", "package_price":"7.5"}),
        ),
    ];
    for (model, expected) in models {
        let b = basket(Operation::Preview, two_lines());
        let all = vec![terms(line_id(1)), terms(line_id(2))];
        let mut bindings = composed(&b);
        let binding = &mut bindings.get_mut(&line_id(1)).unwrap()[0];
        binding.price.model = model;
        binding.kind = ChargeKind::Usage;
        binding.meter = Some(MeterRef {
            usage_type_id: "cpu-hours".into(),
            version: "meter-7".into(),
        });
        binding.usage_rating_policy = Some(UsageRatingPolicy {
            policy_id: Uuid::from_u128(991),
            version: 23,
            digest: [0x13; 32],
            content: UsageRatingPolicyInput {
                rating_window: RatingWindow::BillingCycle,
                aggregation_scope: AggregationScope::Resource,
                reset: Reset::RatingWindowStart,
                partial_window: PartialWindow::ActualQuantityFullThresholds,
                fold: Fold::Sum,
            },
        });
        let policy = AcceptancePolicy {
            version: "37".into(),
            hold_seconds: 3600,
        };
        let request = evaluation_request(&b, &bindings, Some(&all), Some(&market()), Some(&policy))
            .unwrap()
            .unwrap();
        let native = request.lines[0].items[0].chains[0]
            .binding
            .native_binding
            .as_ref()
            .unwrap();
        assert_eq!(native["price"]["model"], expected);
        assert_eq!(native["price"]["money_digest"], "07".repeat(32));
        assert_eq!(
            native["meter"],
            json!({"usage_type_id":"cpu-hours", "version":"meter-7"})
        );
        assert_eq!(native["usage_rating_policy"]["version"], "23");
        assert_eq!(native["usage_rating_policy"]["digest"], "13".repeat(32));
        assert_eq!(
            native["usage_rating_policy"]["content"]["aggregation_scope"],
            "resource"
        );
        assert_eq!(request.acceptance_policy, Some(policy));
    }
}

#[test]
fn gate_target_order_id_follows_operation() {
    let order = Uuid::from_u128(77);
    let cases = [
        (GateTarget::Preview, Operation::Preview, None),
        (
            GateTarget::Submit { order_id: order },
            Operation::Submit,
            Some(order),
        ),
        (
            GateTarget::Amendment { order_id: order },
            Operation::Amendment,
            Some(order),
        ),
    ];
    for (target, operation, order_id) in cases {
        assert_eq!(target.operation(), operation, "{target:?} operation");
        assert_eq!(target.order_id(), order_id, "{target:?} order id");
    }
}

/// R3: a terms answer that states one line twice (the readers would disagree on which entry
/// wins) or names an unasked line is malformed; an omitted line stays per-line missing terms.
#[test]
fn a_terms_answer_naming_a_line_twice_or_an_unasked_line_is_malformed() {
    use bss_orders::domain::gate::orchestrate::verify_terms;
    let b = basket(Operation::Submit, two_lines());
    let queries = plan(&b).terms;
    let exact = vec![terms(line_id(1)), terms(line_id(2))];
    assert_eq!(verify_terms(&queries, &exact), Ok(()));
    assert_eq!(
        verify_terms(&queries, &exact[..1]),
        Ok(()),
        "omission is per line"
    );
    let mut twice = exact.clone();
    let mut second = terms(line_id(1));
    second.term = EvaluationTerm::FixedPeriods { count: 24 };
    twice.push(second);
    assert_eq!(verify_terms(&queries, &twice), Err(Unavailable::Malformed));
    let mut foreign = exact;
    foreign.push(terms(line_id(9)));
    assert_eq!(
        verify_terms(&queries, &foreign),
        Err(Unavailable::Malformed)
    );
}

/// R5: the hold policy version is frozen as a canonical positive integer (D-191); any other
/// text is a malformed policy answer, never a 500 at submit.
#[test]
fn a_policy_version_that_is_not_a_canonical_positive_integer_is_malformed() {
    use bss_orders::domain::gate::orchestrate::verify_policy;
    let policy = |version: &str| AcceptancePolicy {
        version: version.into(),
        hold_seconds: 600,
    };
    assert_eq!(verify_policy(&policy("7")), Ok(()));
    for bad in ["07", "+7", "v1", "dev-stub-1", "0", "", "-1"] {
        assert_eq!(
            verify_policy(&policy(bad)),
            Err(Unavailable::Malformed),
            "{bad:?}"
        );
    }
}

/// R5 (OQ-C2, D-231 item 12, D-227 item 4): a bundle line whose items state different quantities
/// is an unsupported purchase shape: `reference-unresolvable` at assessment on every
/// operation, never a 500 at submit. Each quantity alone passes the floor.
#[test]
fn a_bundle_line_with_mixed_item_quantities_refuses_reference_unresolvable() {
    for operation in [Operation::Preview, Operation::Submit] {
        let mut line = two_lines().remove(0);
        line.catalog.items[1].quantity = "2".into();
        let b = basket(operation, vec![line]);
        let out = assess(&assemble(&b, answered(&b))).unwrap();
        let find = |p: PredicateId| {
            out.vector
                .outcomes()
                .iter()
                .find(|r| r.key.predicate == p)
                .unwrap()
                .clone()
        };
        let reference = find(PredicateId::ReferenceResolution);
        assert_eq!(reference.verdict, Verdict::Failed, "{operation:?}");
        assert_eq!(
            reference.reason.map(bss_orders_sdk::catalog::Reason::token),
            Some("reference-unresolvable")
        );
        assert_eq!(
            reference
                .upstream_detail
                .as_ref()
                .and_then(|d| d.upstream_code.as_deref()),
            Some("mixed-item-quantities")
        );
        assert_eq!(find(PredicateId::QuantityFloor).verdict, Verdict::Passed);
    }
    // Equal values in different canonical spellings cannot occur (the floor requires
    // canonical text), and one quantity on every item still passes.
    let b = basket(Operation::Submit, vec![two_lines().remove(0)]);
    let out = assess(&assemble(&b, answered(&b))).unwrap();
    assert!(out.vector.outcomes().iter().any(|r| r.key.predicate
        == PredicateId::ReferenceResolution
        && r.verdict == Verdict::Passed));
}
