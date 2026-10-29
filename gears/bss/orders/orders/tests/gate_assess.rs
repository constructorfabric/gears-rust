#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once; `allow`, not `expect`, because not every item unwraps or expects"
)]
//! The pure gate assessment over typed port answers (S3-09 skeleton; F03 §3.1–§3.4, §4.2;
//! D-187, D-189, D-195, D-227): one test per rule (a)–(l).
use bss_orders::domain::contributions::AggregateFacts;
use bss_orders::domain::dates::{
    DateBasis, DatePolicySnapshot, PLATFORM_DEFAULT_POLICY_ID, PolicyRow, SuppliedDates,
};
use bss_orders::domain::gate::answers::*;
use bss_orders::domain::gate::assess::{Assessment, AssessmentInput, BasketLine, assess};
use bss_orders::domain::gate::orchestrate::TenantAxes;
use bss_orders::domain::gate::ports::{GatePort, PortAnswer, Unavailable};
use bss_orders::domain::gate::predicates::{PREDICATES, PredicateId, Scope};
use bss_orders::domain::gate::pricing_map::*;
use bss_orders::domain::gate::vector::{
    Applicability, Failure, Operation, Outcome, ProducerResult, Vector, Verdict,
};
use bss_orders::domain::guards::{ALL_PORTS, GATE_COMPOSITE, GuardRegistry, InputPort};
use bss_orders::domain::state_table::{RowId, StateTable};
use bss_orders::domain::transition::{
    Decision, DecisionError, DraftRevisions, GuardBindings, GuardSubject, GuardVerdict, InputState,
    decide,
};
use bss_orders_sdk::catalog::{OrderState, Reason, Trigger};
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
/// The trusted axes of the protected-profile fixtures: one tenant on every axis.
fn occupancy_axes() -> TenantAxes {
    TenantAxes {
        resource: TENANT,
        payer: TENANT,
        seller: TENANT,
    }
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
        basket_line(2, vec![sel(ITEM, None, "2"), sel(ITEM_B, Some("x"), "2")]),
    ]
}
fn base(op: Operation) -> AssessmentInput {
    input_for(op, two_lines())
}
fn unavailable<T>() -> PortAnswer<T> {
    PortAnswer::Unavailable(Unavailable::Outage)
}

// ---- lookups ----

fn rows(v: &Vector, p: PredicateId) -> Vec<&Outcome> {
    v.outcomes()
        .iter()
        .filter(|o| o.key.predicate == p)
        .collect()
}
fn row(v: &Vector, p: PredicateId, line: Option<Uuid>) -> &Outcome {
    v.outcomes()
        .iter()
        .find(|o| o.key.predicate == p && o.key.line_id == line && o.key.item_id.is_none())
        .unwrap_or_else(|| panic!("no {p:?} row for {line:?}"))
}
fn vr(o: &Outcome) -> (Verdict, Option<Reason>) {
    (o.verdict, o.reason)
}
const PASS: (Verdict, Option<Reason>) = (Verdict::Passed, None);
fn failed(r: Reason) -> (Verdict, Option<Reason>) {
    (Verdict::Failed, Some(r))
}
fn unevaluable(r: Reason) -> (Verdict, Option<Reason>) {
    (Verdict::Unevaluable, Some(r))
}

// ---- (a) completeness ----

#[test]
fn a_every_predicate_has_a_row_per_applicable_scope() {
    let a = assess(&base(Operation::Submit)).unwrap();
    let lines = two_lines();
    let slots: usize = lines.iter().map(|l| l.catalog.items.len()).sum();
    for spec in &PREDICATES {
        let n = rows(&a.vector, spec.id).len();
        let expected = match spec.scope {
            Scope::Order => 1,
            Scope::Line => lines.len(),
            Scope::Slot => slots,
        };
        assert_eq!(n, expected, "{:?}", spec.id);
    }
    assert!(
        a.vector
            .outcomes()
            .iter()
            .all(|o| o.verdict == Verdict::Passed),
        "{:#?}",
        a.vector.failures().collect::<Vec<_>>()
    );
    assert!(a.failures.is_empty());
    assert_eq!(a.primary, None);
    assert!(matches!(a.pins[&line_id(1)], PinOutcome::Composed { .. }));
    assert!(matches!(a.pins[&line_id(2)], PinOutcome::Composed { .. }));

    // An empty basket yields the order-level rows only.
    let empty = assess(&input_for(Operation::Preview, Vec::new())).unwrap();
    let preds: Vec<PredicateId> = empty
        .vector
        .outcomes()
        .iter()
        .map(|o| o.key.predicate)
        .collect();
    assert_eq!(
        preds,
        [
            PredicateId::AxisValidity,
            PredicateId::ContractActive,
            PredicateId::SingleCurrency
        ]
    );
    assert!(empty.pins.is_empty());
}

// ---- (b) unavailable ports and engine inputs ----

#[test]
fn b_unavailable_port_makes_its_predicates_unevaluable_and_its_inputs_unresolvable() {
    b_answered();
    b_identity();
    b_contracts();
    b_overlap_key();
    b_occupancy();
    b_resolve();
    b_no_catalog();
    b_pin_ports();
    b_evaluation();
    b_all_down();
}

fn b_answered() {
    // Every port answered: every fed input resolved; nothing records AcceptanceRequirement.
    let a = assess(&base(Operation::Submit)).unwrap();
    for port in GatePort::ALL {
        for input in port.feeds() {
            assert_eq!(
                a.inputs.get(*input),
                Some(InputState::Resolved),
                "{input:?}"
            );
        }
    }
    assert_eq!(a.inputs.get(InputPort::AcceptanceRequirement), None);
}

fn b_identity() {
    let (l1, l2) = (line_id(1), line_id(2));
    // Identity.
    let mut i = base(Operation::Submit);
    i.identity = unavailable();
    let a = assess(&i).unwrap();
    let r = Reason::IdentityPartyUnavailable;
    assert_eq!(
        vr(row(&a.vector, PredicateId::AxisValidity, None)),
        unevaluable(r)
    );
    for l in [l1, l2] {
        assert_eq!(
            vr(row(&a.vector, PredicateId::MarketConsistency, Some(l))),
            unevaluable(r)
        );
    }
    assert_eq!(
        a.inputs.get(InputPort::Identity),
        Some(InputState::Unresolvable(r))
    );
    assert_eq!(
        a.inputs.get(InputPort::Occupancy),
        Some(InputState::Resolved)
    );
}

fn b_contracts() {
    // Contracts (read only because a line references a contract).
    let mut i = base(Operation::Submit);
    i.lines[0].contract_ref = Some(Uuid::from_u128(70));
    i.contracts = unavailable();
    let a = assess(&i).unwrap();
    let r = Reason::ContractResolutionUnavailable;
    assert_eq!(
        vr(row(&a.vector, PredicateId::ContractActive, None)),
        unevaluable(r)
    );
    assert_eq!(
        a.inputs.get(InputPort::ContractResolution),
        Some(InputState::Unresolvable(r))
    );
}

fn b_overlap_key() {
    let (l1, l2) = (line_id(1), line_id(2));
    // Overlap key.
    let mut i = base(Operation::Submit);
    i.overlap_keys = unavailable();
    let a = assess(&i).unwrap();
    let r = Reason::OverlapKeyUnavailable;
    for l in [l1, l2] {
        assert_eq!(
            vr(row(&a.vector, PredicateId::OverlapUniqueness, Some(l))),
            unevaluable(r)
        );
        assert_eq!(
            vr(row(&a.vector, PredicateId::InFlightPerKey, Some(l))),
            unevaluable(r)
        );
    }
    assert_eq!(
        a.inputs.get(InputPort::OverlapKey),
        Some(InputState::Unresolvable(r))
    );
}

/// A protected observation with no own membership (the assessment asks for none).
fn protected_row(key: &str, revision: &str) -> (OccupancyQuery, OccupancyObservation) {
    let (query, _) = occupancy(key, 0, MaxConcurrentActive::Unlimited);
    let observation = serde_json::from_value(serde_json::json!({
        "profile": "protected_full_tuple_v1", "demand_unit": "line_scope_v1",
        "observation_revision": revision,
        "scope": {"payer_tenant_id": TENANT, "resource_tenant_id": TENANT, "overlap_scope_key": key},
        "policy": {"source": "catalog", "reference": "policy-1", "version": "1"},
        "active_count": 0, "retained_suspended_count": 0, "order_reserved_count": 0,
        "max_concurrent_active": null, "own_membership": null
    }))
    .unwrap();
    (query, observation)
}

#[test]
fn protected_batch_rejection_carries_its_cause_in_upstream_detail() {
    let (l1, l2) = (line_id(1), line_id(2));
    let mut i = base(Operation::Submit);
    i.occupancy_axes = Some(occupancy_axes());
    i.occupancy = PortAnswer::Answered(vec![
        protected_row(&key_of(l1), "snapshot-1"),
        protected_row(&key_of(l2), "snapshot-2"),
    ]);
    let a = assess(&i).unwrap();
    for l in [l1, l2] {
        let o = row(&a.vector, PredicateId::OverlapUniqueness, Some(l));
        assert_eq!(vr(o), unevaluable(Reason::OverlapPresenceUnevaluable));
        assert_eq!(
            o.upstream_detail
                .as_ref()
                .and_then(|d| d.upstream_code.as_deref()),
            Some("protected-mixed-revisions")
        );
    }
}

fn b_occupancy() {
    let (l1, l2) = (line_id(1), line_id(2));
    // Occupancy.
    let mut i = base(Operation::Submit);
    i.occupancy = unavailable();
    i.inflight_same_tuple.insert(key_of(l1));
    let a = assess(&i).unwrap();
    let r = Reason::OverlapPresenceUnevaluable;
    for l in [l1, l2] {
        assert_eq!(
            vr(row(&a.vector, PredicateId::OverlapUniqueness, Some(l))),
            unevaluable(r)
        );
        assert_eq!(
            vr(row(&a.vector, PredicateId::InFlightPerKey, Some(l))),
            if l == l1 {
                failed(Reason::OrderInFlightForKey)
            } else {
                PASS
            }
        );
    }
    assert_eq!(
        a.inputs.get(InputPort::Occupancy),
        Some(InputState::Unresolvable(r))
    );
}

fn b_resolve() {
    let l1 = line_id(1);
    // Resolve, for one line: the catalog inputs and the pin input are unresolvable, each with
    // its own input's reason (D-227 item 1), never the port's primary reason for both.
    let mut i = base(Operation::Submit);
    let c = i.catalog.get_mut(&l1).unwrap();
    c.resolved = Err(ReadFailure::Unavailable(Unavailable::Deadline));
    let a = assess(&i).unwrap();
    assert_eq!(
        a.inputs.get(InputPort::CatalogPredicates),
        Some(InputState::Unresolvable(
            Reason::CatalogPredicatesUnavailable
        ))
    );
    assert_eq!(
        a.inputs.get(InputPort::PinComposition),
        Some(InputState::Unresolvable(
            Reason::CatalogPinCompositionUnavailable
        ))
    );
    assert_eq!(
        a.inputs.get(InputPort::PricingRevision),
        Some(InputState::Resolved)
    );
}

fn b_no_catalog() {
    let l2 = line_id(2);
    // A line without any catalog answer: both Pricing ports did not answer.
    let mut i = base(Operation::Submit);
    i.catalog.remove(&l2);
    let a = assess(&i).unwrap();
    assert_eq!(
        vr(row(
            &a.vector,
            PredicateId::CatalogRevisionCurrent,
            Some(l2)
        )),
        unevaluable(Reason::PricingRevisionUnavailable)
    );
    assert_eq!(
        a.inputs.get(InputPort::PricingRevision),
        Some(InputState::Unresolvable(Reason::PricingRevisionUnavailable))
    );
}

fn b_pin_ports() {
    let l1 = line_id(1);
    // Billing terms and acceptance policy feed the pin input.
    for which in 0..2 {
        let mut i = base(Operation::Submit);
        if which == 0 {
            i.billing_terms = unavailable();
        } else {
            i.acceptance_policy = unavailable();
        }
        let a = assess(&i).unwrap();
        let r = Reason::CatalogPinCompositionUnavailable;
        assert_eq!(
            vr(row(&a.vector, PredicateId::PinComposition, Some(l1))),
            unevaluable(r)
        );
        assert_eq!(
            a.inputs.get(InputPort::PinComposition),
            Some(InputState::Unresolvable(r))
        );
        assert_eq!(
            a.inputs.get(InputPort::CatalogPredicates),
            Some(InputState::Resolved)
        );
    }
}

fn b_evaluation() {
    // Evaluation feeds only the engine input; no row reads it.
    let mut i = base(Operation::Submit);
    i.evaluation = unavailable();
    let a = assess(&i).unwrap();
    assert_eq!(
        a.inputs.get(InputPort::Evaluation),
        Some(InputState::Unresolvable(Reason::EvaluationUnavailable))
    );
    assert!(a.vector.failures().next().is_none());
}

fn b_all_down() {
    // Every port unavailable at once: every fed input is unresolvable with its own reason.
    let mut i = base(Operation::Submit);
    i.identity = unavailable();
    i.contracts = unavailable();
    i.overlap_keys = unavailable();
    i.occupancy = unavailable();
    i.billing_terms = unavailable();
    i.acceptance_policy = unavailable();
    i.assessment = unavailable();
    i.evaluation = unavailable();
    i.catalog = BTreeMap::new();
    let a = assess(&i).unwrap();
    for port in GatePort::ALL {
        for (input, reason) in port.input_reasons() {
            assert_eq!(a.inputs.get(input), Some(InputState::Unresolvable(reason)));
        }
    }
    // The outage never drops a row: each line keeps the happy fixture's row count.
    let happy = assess(&base(Operation::Submit)).unwrap();
    let per_line = |v: &Vector, l| v.outcomes().iter().filter(|o| o.key.line_id == l).count();
    for l in [None, Some(line_id(1)), Some(line_id(2))] {
        assert_eq!(per_line(&a.vector, l), per_line(&happy.vector, l), "{l:?}");
    }
}

// ---- (c) overlap ----

#[test]
fn c_overlap_uniqueness_counts_active_proposed_and_in_flight() {
    let (l1, l2) = (line_id(1), line_id(2));
    let k1 = key_of(l1);
    let uniq = |i: &AssessmentInput, l| {
        let a = assess(i).unwrap();
        vr(row(&a.vector, PredicateId::OverlapUniqueness, Some(l)))
    };

    // Unlimited passes whatever the count.
    let mut i = base(Operation::Submit);
    i.occupancy = PortAnswer::Answered(vec![
        occupancy(&k1, 900, MaxConcurrentActive::Unlimited),
        occupancy(&key_of(l2), 0, MaxConcurrentActive::Limit(1)),
    ]);
    i.inflight_other_orders.insert(k1.clone(), 5);
    i.inflight_same_tuple.insert(k1.clone());
    assert_eq!(uniq(&i, l1), PASS);
    assert_eq!(
        vr(row(
            &assess(&i).unwrap().vector,
            PredicateId::InFlightPerKey,
            Some(l1)
        )),
        failed(Reason::OrderInFlightForKey)
    );

    // active 1 + proposed 1 + in-flight 0 <= 2 passes; one more in flight fails.
    i.occupancy = PortAnswer::Answered(vec![
        occupancy(&k1, 1, MaxConcurrentActive::Limit(2)),
        occupancy(&key_of(l2), 0, MaxConcurrentActive::Limit(1)),
    ]);
    i.inflight_other_orders.clear();
    assert_eq!(uniq(&i, l1), PASS);
    i.inflight_other_orders.insert(k1.clone(), 1);
    assert_eq!(uniq(&i, l1), failed(Reason::OverlapCardinalityExceeded));
    assert_eq!(uniq(&i, l2), PASS);

    // Two basket lines on one key count twice.
    let mut i = base(Operation::Submit);
    i.overlap_keys =
        PortAnswer::Answered([overlap_keys(l1, "shared"), overlap_keys(l2, "shared")].concat());
    i.occupancy = PortAnswer::Answered(vec![occupancy("shared", 0, MaxConcurrentActive::Limit(1))]);
    assert_eq!(uniq(&i, l1), failed(Reason::OverlapCardinalityExceeded));
    assert_eq!(uniq(&i, l2), failed(Reason::OverlapCardinalityExceeded));
    i.occupancy = PortAnswer::Answered(vec![occupancy("shared", 0, MaxConcurrentActive::Limit(2))]);
    assert_eq!(uniq(&i, l1), PASS);

    // A key the occupancy answer does not cover: never default to a limit.
    let mut i = base(Operation::Submit);
    i.occupancy = PortAnswer::Answered(vec![occupancy(&k1, 0, MaxConcurrentActive::Limit(1))]);
    assert_eq!(uniq(&i, l1), PASS);
    assert_eq!(
        uniq(&i, l2),
        unevaluable(Reason::OverlapPresenceUnevaluable)
    );

    // A line the key answer does not cover: the key is unresolvable on both predicates.
    let mut i = base(Operation::Submit);
    i.overlap_keys = PortAnswer::Answered(overlap_keys(l1, &k1));
    let a = assess(&i).unwrap();
    let r = Reason::OverlapKeyUnresolvable;
    assert_eq!(
        vr(row(&a.vector, PredicateId::OverlapUniqueness, Some(l2))),
        failed(r)
    );
    assert_eq!(
        vr(row(&a.vector, PredicateId::InFlightPerKey, Some(l2))),
        unevaluable(r)
    );
    // The answer is rejected before reads (R8): l1's valid key was never read.
    assert_eq!(
        vr(row(&a.vector, PredicateId::InFlightPerKey, Some(l1))),
        unevaluable(r)
    );

    // A missing key with occupancy down: rule (b) wins on the row that reads occupancy, the
    // missing key still decides the in-flight row.
    i.occupancy = unavailable();
    let a = assess(&i).unwrap();
    assert_eq!(
        vr(row(&a.vector, PredicateId::OverlapUniqueness, Some(l2))),
        unevaluable(Reason::OverlapPresenceUnevaluable)
    );
    assert_eq!(
        vr(row(&a.vector, PredicateId::InFlightPerKey, Some(l2))),
        unevaluable(r)
    );
}

// ---- (d) dates ----

#[test]
fn d_dates_resolvable_follows_the_date_basis() {
    let (l1, l2) = (line_id(1), line_id(2));
    let mut i = base(Operation::Submit);
    // Policy requires both dependent dates; line 1 supplies them, line 2 does not.
    let supplied = SuppliedDates {
        contract_effective: None,
        service_activation: Some(day()),
        acceptance_due: Some(day()),
    };
    i.dates = DateBasis::resolve(
        TENANT,
        policy(true),
        day(),
        [(l1, supplied), (l2, SuppliedDates::default())],
    );
    let a = assess(&i).unwrap();
    assert_eq!(
        vr(row(&a.vector, PredicateId::DatesResolvable, Some(l1))),
        PASS
    );
    assert_eq!(
        vr(row(&a.vector, PredicateId::DatesResolvable, Some(l2))),
        failed(Reason::DateCascadeInvalid)
    );
    // Two missing fields, one row, one failure.
    let dc: Vec<&Failure> = a
        .failures
        .iter()
        .filter(|f| f.reason == Reason::DateCascadeInvalid)
        .collect();
    assert_eq!(dc.len(), 1);
    assert_eq!(dc[0].line_id, Some(l2));

    // A basis covering no line is a programming error: every line is unevaluable, never passed.
    i.dates = dates_for(&[], false);
    let a = assess(&i).unwrap();
    for l in [l1, l2] {
        assert_eq!(
            vr(row(&a.vector, PredicateId::DatesResolvable, Some(l))),
            unevaluable(Reason::DateCascadeInvalid)
        );
    }
    // A basis that does not cover a line leaves that line unevaluable too.
    let lines = two_lines();
    i.dates = dates_for(&lines[..1], false);
    let a = assess(&i).unwrap();
    assert_eq!(
        vr(row(&a.vector, PredicateId::DatesResolvable, Some(l1))),
        PASS
    );
    assert_eq!(
        vr(row(&a.vector, PredicateId::DatesResolvable, Some(l2))),
        unevaluable(Reason::DateCascadeInvalid)
    );
}

// ---- (e) axis validity ----

#[test]
fn e_axis_validity_reads_the_payer_profile() {
    let axis = |p: PayerProfile| {
        let mut i = base(Operation::Submit);
        i.identity = PortAnswer::Answered(p);
        let a = assess(&i).unwrap();
        row(&a.vector, PredicateId::AxisValidity, None).clone()
    };
    assert_eq!(vr(&axis(profile())), PASS);

    let o = axis(PayerProfile {
        axes_valid: false,
        ..profile()
    });
    assert_eq!(vr(&o), failed(Reason::AxisInvalid));
    assert_eq!(o.upstream_detail, None);

    for (seller, resource) in [(false, true), (true, false)] {
        let o = axis(PayerProfile {
            in_relationship_with_seller: seller,
            in_relationship_with_resource: resource,
            ..profile()
        });
        // D-228: the registered reason, not `axis-invalid` with an upstream code.
        assert_eq!(vr(&o), failed(Reason::PayerRelationshipUnconfirmed));
        assert_eq!(o.upstream_detail, None);
    }
}

// ---- (f) contract ----

#[test]
fn f_contract_active_reads_contracts_only_when_referenced() {
    // No reference: passed even though the port did not answer.
    let mut i = base(Operation::Submit);
    i.contracts = unavailable();
    let a = assess(&i).unwrap();
    assert_eq!(vr(row(&a.vector, PredicateId::ContractActive, None)), PASS);
    assert_eq!(
        a.inputs.get(InputPort::ContractResolution),
        Some(InputState::Unresolvable(
            Reason::ContractResolutionUnavailable
        ))
    );

    let c = Uuid::from_u128(70);
    let facts = |status, payer_eligible| ContractFacts {
        status,
        payer_eligible,
        effective_at: None,
        acceptance_required: None,
        version: "c1".into(),
    };
    let contract = |answer: BTreeMap<Uuid, ContractFacts>| {
        let mut i = base(Operation::Submit);
        i.lines[1].contract_ref = Some(c);
        i.contracts = PortAnswer::Answered(answer);
        let a = assess(&i).unwrap();
        vr(row(&a.vector, PredicateId::ContractActive, None))
    };
    assert_eq!(
        contract(BTreeMap::from([(c, facts(ContractStatus::Active, true))])),
        PASS
    );
    assert_eq!(
        contract(BTreeMap::from([(c, facts(ContractStatus::Inactive, true))])),
        failed(Reason::ContractNotActive)
    );
    assert_eq!(
        contract(BTreeMap::from([(c, facts(ContractStatus::Active, false))])),
        failed(Reason::ContractPartyIneligible)
    );
    // Answered without the referenced contract: not active, never passed.
    assert_eq!(contract(BTreeMap::new()), failed(Reason::ContractNotActive));
}

#[test]
fn order_contract_is_checked_once_at_order_scope() {
    let c = Uuid::from_u128(70);
    for op in [Operation::Submit, Operation::Preview] {
        for (status, payer_eligible, expected) in [
            (ContractStatus::Active, true, PASS),
            (
                ContractStatus::Inactive,
                true,
                failed(Reason::ContractNotActive),
            ),
            (
                ContractStatus::Active,
                false,
                failed(Reason::ContractPartyIneligible),
            ),
        ] {
            let mut i = base(op);
            i.contract_id = Some(c);
            i.contracts = PortAnswer::Answered(BTreeMap::from([(
                c,
                ContractFacts {
                    status,
                    payer_eligible,
                    effective_at: None,
                    acceptance_required: None,
                    version: "c1".into(),
                },
            )]));
            let a = assess(&i).unwrap();
            let rows: Vec<_> = a
                .vector
                .outcomes()
                .iter()
                .filter(|o| o.key.predicate == PredicateId::ContractActive)
                .collect();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].key.line_id, None);
            assert_eq!(vr(rows[0]), expected);
        }
    }
}

#[test]
fn order_contract_port_outage_fails_closed() {
    let mut i = base(Operation::Submit);
    i.contract_id = Some(Uuid::from_u128(70));
    i.contracts = unavailable();
    let a = assess(&i).unwrap();
    assert_eq!(
        vr(row(&a.vector, PredicateId::ContractActive, None)),
        unevaluable(Reason::ContractResolutionUnavailable)
    );
    assert!(
        a.failures
            .iter()
            .any(|f| f.line_id.is_none() && f.reason == Reason::ContractResolutionUnavailable)
    );
}

#[test]
fn order_and_line_contracts_are_both_checked() {
    let mut i = base(Operation::Submit);
    let order = Uuid::from_u128(70);
    let line = Uuid::from_u128(71);
    i.contract_id = Some(order);
    i.lines[0].contract_ref = Some(line);
    i.contracts = PortAnswer::Answered(BTreeMap::from([(
        line,
        ContractFacts {
            status: ContractStatus::Active,
            payer_eligible: true,
            effective_at: None,
            acceptance_required: None,
            version: "c1".into(),
        },
    )]));
    let a = assess(&i).unwrap();
    assert_eq!(
        vr(row(&a.vector, PredicateId::ContractActive, None)),
        failed(Reason::ContractNotActive)
    );
}

// ---- (g) currency ----

#[test]
fn g_single_currency_fails_on_two_currencies() {
    let currency = |a: Option<&str>, b: Option<&str>| {
        let mut i = base(Operation::Submit);
        i.lines[0].currency = a.map(str::to_owned);
        i.lines[1].currency = b.map(str::to_owned);
        let a = assess(&i).unwrap();
        vr(row(&a.vector, PredicateId::SingleCurrency, None))
    };
    assert_eq!(currency(Some("USD"), Some("USD")), PASS);
    assert_eq!(currency(Some("USD"), None), PASS);
    assert_eq!(
        currency(Some("USD"), Some("EUR")),
        failed(Reason::CurrencyMixed)
    );
}

// ---- (h) quantity ----

#[test]
fn h_quantity_floor_requires_positive_canonical_decimal_text() {
    let quantity = |q: &str| {
        let mut i = base(Operation::Submit);
        i.lines[1].catalog.items[1].quantity = q.into();
        let a = assess(&i).unwrap();
        (
            vr(row(&a.vector, PredicateId::QuantityFloor, Some(line_id(2)))),
            vr(row(&a.vector, PredicateId::QuantityFloor, Some(line_id(1)))),
        )
    };
    for ok in ["1", "0.5", "12", "1000000.25"] {
        assert_eq!(quantity(ok), (PASS, PASS), "{ok}");
    }
    for bad in [
        "0", "-1", "1.0", "01", "1e2", "+1", " 1", "", "abc", "-0", "0.00",
    ] {
        assert_eq!(
            quantity(bad),
            (failed(Reason::QuantityBelowFloor), PASS),
            "{bad:?}"
        );
    }
}

// ---- (i) the assessment port ----

#[test]
fn i_assessment_answer_composes_predicates_2_7_8_and_market_consistency() {
    let (l1, l2) = (line_id(1), line_id(2));
    let lines = two_lines();
    let slot = &lines[1].catalog.items[1];
    let dim_row = |v: &Vector| {
        v.outcomes()
            .iter()
            .find(|o| {
                o.key.predicate == PredicateId::CatalogDimensionMarket
                    && o.key.line_id == Some(l2)
                    && o.key.item_id == Some(ITEM_B)
            })
            .unwrap()
            .clone()
    };

    // Answered and passing: the rows keep the producer subresults.
    let a = assess(&base(Operation::Submit)).unwrap();
    let o = row(&a.vector, PredicateId::CatalogRevisionAvailable, Some(l1));
    assert_eq!(vr(o), PASS);
    assert_eq!(o.producer_results.len(), 1);
    assert_eq!(o.producer_results[0].producer, "pricing");

    // A producer failure with a registered reason; a second subresult on the same row.
    let mut i = base(Operation::Submit);
    let mut ans = passing_assessment(&lines);
    ans.results.push(keyed(
        l1,
        None,
        ProducerResult {
            producer: "pricing".into(),
            profile: "assess/0".into(),
            ..result(
                PredicateId::CatalogEligibilityRules.token(),
                Verdict::Failed,
                Some("catalog-predicate-failed"),
            )
        },
    ));
    ans.results.push(keyed(
        l2,
        Some(slot),
        ProducerResult {
            profile: "assess/0".into(),
            ..result(
                PredicateId::CatalogDimensionMarket.token(),
                Verdict::Unevaluable,
                Some("catalog-predicate-unevaluable"),
            )
        },
    ));
    i.assessment = PortAnswer::Answered(ans);
    let a = assess(&i).unwrap();
    let o = row(&a.vector, PredicateId::CatalogEligibilityRules, Some(l1));
    assert_eq!(vr(o), failed(Reason::CatalogPredicateFailed));
    assert_eq!(o.producer_results.len(), 2);
    assert_eq!(
        vr(&dim_row(&a.vector)),
        unevaluable(Reason::CatalogPredicateUnevaluable)
    );

    // An unmapped producer reason is invalid coverage (D-195).
    let mut i = base(Operation::Submit);
    let mut ans = passing_assessment(&lines);
    let mc = ans
        .results
        .iter_mut()
        .find(|r| r.line_id == l2 && r.result.predicate == PredicateId::MarketConsistency.token())
        .unwrap();
    mc.result.verdict = Verdict::Failed;
    mc.result.reason = Some("market-is-odd".into());
    i.assessment = PortAnswer::Answered(ans);
    let a = assess(&i).unwrap();
    let o = row(&a.vector, PredicateId::MarketConsistency, Some(l2));
    assert_eq!(vr(o), unevaluable(Reason::CatalogPredicatesUnavailable));

    // A row the answer does not cover; out-of-context entries are discarded and make the
    // envelope malformed, so no row it would pass is admitted (D-195).
    let mut i = base(Operation::Submit);
    let mut ans = passing_assessment(&lines);
    ans.results.retain(|r| {
        !(r.line_id == l2
            && r.result.predicate == PredicateId::CatalogDimensionMarket.token()
            && r.item_id == Some(ITEM_B))
    });
    ans.results.push(keyed(
        line_id(99),
        None,
        result(
            PredicateId::CatalogRevisionAvailable.token(),
            Verdict::Passed,
            None,
        ),
    ));
    ans.results.push(keyed(
        l1,
        None,
        result(
            "pricing.unknown-predicate",
            Verdict::Failed,
            Some("catalog-predicate-failed"),
        ),
    ));
    i.assessment = PortAnswer::Answered(ans);
    let a = assess(&i).unwrap();
    assert_eq!(
        vr(&dim_row(&a.vector)),
        unevaluable(Reason::CatalogPredicatesUnavailable)
    );
    // D-195: the verifiable l1 entry passed, but a malformed envelope never admits it.
    let o = row(&a.vector, PredicateId::CatalogRevisionAvailable, Some(l1));
    assert_eq!(vr(o), unevaluable(Reason::CatalogPredicatesUnavailable));
    assert_eq!(
        o.upstream_detail
            .as_ref()
            .and_then(|d| d.upstream_code.as_deref()),
        Some("assessment-envelope-malformed")
    );
    assert_eq!(
        o.producer_results.len(),
        1,
        "the verifiable evidence is kept"
    );
    assert!(
        a.vector
            .outcomes()
            .iter()
            .all(|o| o.key.line_id != Some(line_id(99)))
    );

    i_malformed_envelope_never_admits_a_row_d195();

    // The port did not answer: 2, 7, 8 carry the port reason; MarketConsistency's owner row is
    // missing (F03 §4.2 predicate 4).
    let mut i = base(Operation::Submit);
    i.assessment = unavailable();
    let a = assess(&i).unwrap();
    let r = Reason::CatalogPredicatesUnavailable;
    for l in [l1, l2] {
        assert_eq!(
            vr(row(
                &a.vector,
                PredicateId::CatalogRevisionAvailable,
                Some(l)
            )),
            unevaluable(r)
        );
        assert_eq!(
            vr(row(
                &a.vector,
                PredicateId::CatalogEligibilityRules,
                Some(l)
            )),
            unevaluable(r)
        );
        assert_eq!(
            vr(row(&a.vector, PredicateId::MarketConsistency, Some(l))),
            unevaluable(Reason::CatalogPredicateUnevaluable)
        );
    }
    for o in rows(&a.vector, PredicateId::CatalogDimensionMarket) {
        assert_eq!(vr(o), unevaluable(r));
        assert!(o.producer_results.is_empty());
    }
}

/// D-195: any unknown-token, out-of-basket, out-of-slot or duplicate-identity entry makes the
/// envelope malformed: would-be passed rows become `catalog-predicates-unavailable`, non-passed
/// rows keep their composed verdict; a well-formed envelope still passes.
fn i_malformed_envelope_never_admits_a_row_d195() {
    let (l1, l2) = (line_id(1), line_id(2));
    let lines = two_lines();
    let revision = |a: &bss_orders::domain::gate::assess::Assessment, l| {
        vr(row(
            &a.vector,
            PredicateId::CatalogRevisionAvailable,
            Some(l),
        ))
    };
    let malformed = unevaluable(Reason::CatalogPredicatesUnavailable);

    // Well-formed (no stray entries): every assessed row passes.
    let a = assess(&base(Operation::Submit)).unwrap();
    for p in [
        PredicateId::CatalogRevisionAvailable,
        PredicateId::CatalogEligibilityRules,
        PredicateId::CatalogDimensionMarket,
        PredicateId::MarketConsistency,
    ] {
        assert!(rows(&a.vector, p).iter().all(|o| vr(o) == PASS), "{p:?}");
    }

    let stray: [(&str, ProducerResultWithKey); 3] = [
        // Out of slot: a slot-scoped predicate keyed on a slot line 1 never selected.
        (
            "out-of-slot",
            keyed(
                l1,
                Some(&sel(ITEM_B, Some("zz"), "1")),
                result(
                    PredicateId::CatalogDimensionMarket.token(),
                    Verdict::Passed,
                    None,
                ),
            ),
        ),
        // A predicate the assessment never answers.
        (
            "non-assessed",
            keyed(
                l1,
                None,
                result(PredicateId::QuantityFloor.token(), Verdict::Passed, None),
            ),
        ),
        // A duplicate subresult identity on a covered row.
        (
            "duplicate",
            keyed(
                l2,
                None,
                result(
                    PredicateId::CatalogRevisionAvailable.token(),
                    Verdict::Passed,
                    None,
                ),
            ),
        ),
    ];
    for (what, entry) in stray {
        let mut i = base(Operation::Submit);
        let mut ans = passing_assessment(&lines);
        // A non-passed row keeps its composed verdict.
        let el = ans
            .results
            .iter_mut()
            .find(|r| {
                r.line_id == l2
                    && r.result.predicate == PredicateId::CatalogEligibilityRules.token()
            })
            .unwrap();
        el.result.verdict = Verdict::Failed;
        el.result.reason = Some("catalog-predicate-failed".into());
        ans.results.push(entry);
        i.assessment = PortAnswer::Answered(ans);
        let a = assess(&i).unwrap();
        assert_eq!(revision(&a, l1), malformed, "{what}");
        assert_eq!(
            vr(row(&a.vector, PredicateId::MarketConsistency, Some(l1))),
            malformed,
            "{what}"
        );
        assert_eq!(
            vr(row(
                &a.vector,
                PredicateId::CatalogEligibilityRules,
                Some(l2)
            )),
            failed(Reason::CatalogPredicateFailed),
            "{what}"
        );
        assert!(a.primary.is_some(), "{what}");
    }
}

// ---- (j) pin ----

#[test]
fn j_pin_row_applicability_and_policy() {
    let (l1, l2) = (line_id(1), line_id(2));
    let pin = |a: &bss_orders::domain::gate::assess::Assessment, l| {
        let o = row(&a.vector, PredicateId::PinComposition, Some(l));
        (o.verdict, o.reason, o.applicability)
    };
    // Submit, Amendment, Preview: a composed pin passes as required.
    for op in [Operation::Submit, Operation::Amendment, Operation::Preview] {
        let a = assess(&base(op)).unwrap();
        assert_eq!(
            pin(&a, l1),
            (Verdict::Passed, None, Applicability::Required)
        );
    }

    // Preview without billing terms withholds TCV: tcv-only, never blocking a Preview.
    let r = Some(Reason::CatalogPinCompositionUnavailable);
    let mut i = base(Operation::Preview);
    i.billing_terms = unavailable();
    let a = assess(&i).unwrap();
    assert_eq!(
        pin(&a, l1),
        (Verdict::Unevaluable, r, Applicability::TcvOnly)
    );
    assert_eq!(a.primary, None);
    // Preview with terms for line 1 only.
    i.billing_terms = PortAnswer::Answered(vec![terms(l1)]);
    let a = assess(&i).unwrap();
    assert_eq!(
        pin(&a, l1),
        (Verdict::Passed, None, Applicability::Required)
    );
    assert_eq!(
        pin(&a, l2),
        (Verdict::Unevaluable, r, Applicability::TcvOnly)
    );
    // The same gap on Submit blocks.
    let mut i = base(Operation::Submit);
    i.billing_terms = unavailable();
    let a = assess(&i).unwrap();
    assert_eq!(
        pin(&a, l1),
        (Verdict::Unevaluable, r, Applicability::Required)
    );
    assert_eq!(
        a.primary.unwrap().key.predicate,
        PredicateId::PinComposition
    );

    // An answered-missing policy.
    let m = Some(Reason::OrderBindingPolicyMissing);
    for op in [Operation::Submit, Operation::Amendment] {
        let mut i = base(op);
        i.acceptance_policy = PortAnswer::Answered(None);
        let a = assess(&i).unwrap();
        assert_eq!(pin(&a, l1), (Verdict::Failed, m, Applicability::Required));
    }
    let mut i = base(Operation::Preview);
    i.acceptance_policy = PortAnswer::Answered(None);
    let a = assess(&i).unwrap();
    assert_eq!(
        pin(&a, l1),
        (Verdict::Unevaluable, m, Applicability::PreviewForecast)
    );
    assert_eq!(a.primary, None);

    // The policy did not answer.
    let mut i = base(Operation::Submit);
    i.acceptance_policy = unavailable();
    let a = assess(&i).unwrap();
    assert_eq!(
        pin(&a, l1),
        (Verdict::Unevaluable, r, Applicability::Required)
    );

    // A pin the answers prove cannot be composed (a selected slot unbound) is failed, and an
    // already non-passed pin keeps its reason when the policy did not answer.
    let mut i = base(Operation::Submit);
    i.catalog
        .insert(l1, catalog_answer(vec![cell(ITEM, None, false)]));
    i.acceptance_policy = unavailable();
    let a = assess(&i).unwrap();
    assert_eq!(
        pin(&a, l1),
        (
            Verdict::Failed,
            Some(Reason::PinUnresolvable),
            Applicability::Required
        )
    );
    assert_eq!(
        a.pins[&l1],
        PinOutcome::Unresolvable(Reason::PinUnresolvable)
    );

    // D-195: tcv-only is the terms-withholding path only; an unresolvable pin on a Preview
    // without terms stays required and blocks.
    let mut i = base(Operation::Preview);
    i.catalog
        .insert(l1, catalog_answer(vec![cell(ITEM, None, false)]));
    i.billing_terms = unavailable();
    let a = assess(&i).unwrap();
    assert_eq!(
        pin(&a, l1),
        (
            Verdict::Failed,
            Some(Reason::PinUnresolvable),
            Applicability::Required
        )
    );
    assert!(a.vector.primary(Operation::Preview).is_some());
    assert_eq!(
        pin(&a, l2),
        (Verdict::Unevaluable, r, Applicability::TcvOnly)
    );
}

// ---- (m) a basket line whose catalog request names another line ----

#[test]
fn m_mismatched_line_id_is_reference_unresolvable_never_repaired() {
    let (l1, l2) = (line_id(1), line_id(2));
    let mut i = base(Operation::Submit);
    i.lines[0].catalog.line_id = Uuid::from_u128(999);
    let a = assess(&i).unwrap();
    let o = row(&a.vector, PredicateId::ReferenceResolution, Some(l1));
    assert_eq!(vr(o), failed(Reason::ReferenceUnresolvable));
    assert_eq!(
        o.upstream_detail
            .as_ref()
            .and_then(|d| d.upstream_code.as_deref()),
        Some("basket-line-id-mismatch")
    );
    assert_eq!(
        a.pins[&l1],
        PinOutcome::Unevaluable(Reason::ReferenceUnresolvable)
    );
    let pin = row(&a.vector, PredicateId::PinComposition, Some(l1));
    assert_eq!(vr(pin), unevaluable(Reason::ReferenceUnresolvable));
    // Rows stay keyed by the basket line; nothing is keyed by the request's id.
    assert!(
        a.vector
            .outcomes()
            .iter()
            .all(|o| o.key.line_id != Some(Uuid::from_u128(999)))
    );
    assert_eq!(
        vr(row(&a.vector, PredicateId::ReferenceResolution, Some(l2))),
        PASS
    );
    assert_eq!(
        a.failures
            .iter()
            .filter(|f| f.line_id == Some(l1) && f.reason == Reason::ReferenceUnresolvable)
            .count(),
        1
    );
    assert!(a.primary.is_some());
}

// ---- (k) the failure list ----

#[test]
fn k_failures_are_deduplicated_by_line_and_reason_in_vector_order() {
    let (l1, l2) = (line_id(1), line_id(2));
    let mut i = base(Operation::Submit);
    // Line 1 selects an item the revision does not contain: membership and coverage fail with
    // `catalog-predicate-failed`, the reference is unresolvable and the pin carries that reason.
    i.catalog
        .insert(l1, catalog_answer(vec![cell(ITEM_B, Some("x"), true)]));
    // Only line 2 composes, so only its key is asked and answered (R8: an unasked entry rejects
    // the answer before reads).
    i.overlap_keys = PortAnswer::Answered(overlap_keys(l2, &key_of(l2)));
    // Both lines miss required dates.
    i.dates = dates_for(&two_lines(), true);
    i.lines[1].currency = Some("EUR".into());
    let a = assess(&i).unwrap();

    let r = row(&a.vector, PredicateId::ReferenceResolution, Some(l1));
    assert_eq!(vr(r), failed(Reason::ReferenceUnresolvable));
    assert_eq!(
        vr(row(&a.vector, PredicateId::PinComposition, Some(l1))),
        unevaluable(Reason::ReferenceUnresolvable)
    );

    let got: Vec<(Option<Uuid>, Reason, PredicateId)> = a
        .failures
        .iter()
        .map(|f| (f.line_id, f.reason, f.predicate))
        .collect();
    assert_eq!(
        got,
        [
            (
                Some(l1),
                Reason::CatalogPredicateFailed,
                PredicateId::CatalogItemMembership
            ),
            (
                Some(l1),
                Reason::CatalogPredicateUnevaluable,
                PredicateId::CatalogSkuVersionDated
            ),
            (
                Some(l1),
                Reason::ReferenceUnresolvable,
                PredicateId::ReferenceResolution
            ),
            (None, Reason::CurrencyMixed, PredicateId::SingleCurrency),
            (
                Some(l1),
                Reason::OverlapKeyUnresolvable,
                PredicateId::OverlapUniqueness
            ),
            (
                Some(l1),
                Reason::DateCascadeInvalid,
                PredicateId::DatesResolvable
            ),
            (
                Some(l2),
                Reason::DateCascadeInvalid,
                PredicateId::DatesResolvable
            ),
        ]
    );
    // Every non-passed row is represented, and the first occurrence wins.
    for o in a.vector.failures() {
        let f = a
            .failures
            .iter()
            .find(|f| f.line_id == o.key.line_id && Some(f.reason) == o.reason)
            .unwrap();
        assert!(f.predicate.index() <= o.key.predicate.index());
    }
    // The primary is the first definitive local refusal (D-230), ahead of the earlier
    // unevaluable catalog row that D-195 alone would pick.
    let primary = a.primary.unwrap();
    assert_eq!(primary.key.predicate, PredicateId::ReferenceResolution);
    assert_eq!(primary.reason, Some(Reason::ReferenceUnresolvable));
}

// ---- (l) determinism ----

fn mixed() -> AssessmentInput {
    let lines = vec![
        basket_line(1, vec![sel(ITEM, None, "1"), sel(ITEM_B, Some("x"), "1.0")]),
        basket_line(2, vec![sel(ITEM, None, "2")]),
        basket_line(3, vec![sel(ITEM_B, Some("x"), "0"), sel(ITEM, None, "0")]),
        basket_line(4, vec![sel(ITEM, None, "4")]),
    ];
    let mut i = input_for(Operation::Submit, lines);
    let (l1, l2, l3, l4) = (line_id(1), line_id(2), line_id(3), line_id(4));
    i.catalog
        .insert(l2, catalog_answer(vec![cell(ITEM, None, false)]));
    i.lines[2].currency = Some("EUR".into());
    i.lines[3].contract_ref = Some(Uuid::from_u128(70));
    i.overlap_keys = PortAnswer::Answered(
        [
            overlap_keys(l1, "shared"),
            overlap_keys(l2, "shared"),
            overlap_keys(l3, "solo"),
        ]
        .concat(),
    );
    i.occupancy = PortAnswer::Answered(vec![
        occupancy("shared", 1, MaxConcurrentActive::Limit(2)),
        occupancy("solo", 0, MaxConcurrentActive::Unlimited),
    ]);
    i.inflight_other_orders.insert("solo".into(), 3);
    i.billing_terms = PortAnswer::Answered(vec![terms(l1), terms(l3)]);
    i.dates = DateBasis::resolve(
        TENANT,
        policy(true),
        day(),
        [
            (l1, SuppliedDates::default()),
            (
                l4,
                SuppliedDates {
                    contract_effective: None,
                    service_activation: Some(day()),
                    acceptance_due: Some(day()),
                },
            ),
        ],
    );
    let mut ans = passing_assessment(&i.lines);
    ans.results.push(keyed(
        l3,
        None,
        ProducerResult {
            profile: "assess/0".into(),
            ..result(
                PredicateId::CatalogRevisionAvailable.token(),
                Verdict::Failed,
                Some("catalog-predicate-failed"),
            )
        },
    ));
    i.assessment = PortAnswer::Answered(ans);
    i
}
/// Reorder every order-carrying input of `i` by permutation `n`.
fn shuffled(i: &AssessmentInput, n: usize) -> AssessmentInput {
    fn perm<T: Clone>(v: &[T], n: usize) -> Vec<T> {
        let mut v = v.to_vec();
        if !v.is_empty() {
            let k = n % v.len();
            v.rotate_left(k);
        }
        if n % 2 == 1 {
            v.reverse();
        }
        v
    }
    let mut s = AssessmentInput {
        occupancy_axes: None,
        unsupported_terms: i.unsupported_terms.clone(),
        lines: perm(&i.lines, n),
        catalog: perm(&i.catalog.clone().into_iter().collect::<Vec<_>>(), n + 1)
            .into_iter()
            .collect(),
        inflight_same_tuple: i.inflight_same_tuple.clone(),
        inflight_other_orders: perm(
            &i.inflight_other_orders
                .clone()
                .into_iter()
                .collect::<Vec<_>>(),
            n,
        )
        .into_iter()
        .collect(),
        ..i.clone()
    };
    if let PortAnswer::Answered(v) = &i.overlap_keys {
        s.overlap_keys = PortAnswer::Answered(perm(v, n + 2));
    }
    if let PortAnswer::Answered(v) = &i.occupancy {
        s.occupancy = PortAnswer::Answered(perm(v, n + 3));
    }
    if let PortAnswer::Answered(v) = &i.billing_terms {
        s.billing_terms = PortAnswer::Answered(perm(v, n));
    }
    if let PortAnswer::Answered(a) = &i.assessment {
        let mut a = a.clone();
        a.results = perm(&a.results, n + 5);
        s.assessment = PortAnswer::Answered(a);
    }
    s
}

#[test]
fn l_shuffled_inputs_yield_a_byte_identical_vector_and_failure_list() {
    let i = mixed();
    let first = assess(&i).unwrap();
    // The fixture exercises failures on several predicates and lines.
    assert!(first.failures.len() >= 6, "{:#?}", first.failures);
    let id = Uuid::nil();
    let public = serde_json::to_string(&first.report(id).outcomes).unwrap();
    for n in 0..50 {
        let a = assess(&shuffled(&i, n)).unwrap();
        assert_eq!(
            serde_json::to_string(&a.report(id).outcomes).unwrap(),
            public,
            "n={n}"
        );
        assert_eq!(a.failures, first.failures, "n={n}");
        assert_eq!(a.vector, first.vector, "n={n}");
        assert_eq!(a.inputs, first.inputs, "n={n}");
        assert_eq!(a.primary, first.primary, "n={n}");
    }
    // The public projection carries tokens, never producer results or detail.
    let v: serde_json::Value = serde_json::from_str(&public).unwrap();
    let o = &v[0];
    assert_eq!(o["predicate"], "catalog.revision-current");
    assert_eq!(o["verdict"], "passed");
    assert_eq!(o["applicability"], "required");
    assert!(o.get("producer_results").is_none());
    assert!(o.get("upstream_detail").is_none());
}

/// D-229: the refusal's `context.data.assessment` report is the public vector and the failure
/// list 1:1, as the same tokens, under the mapping version, and it survives the SDK diagnostics.
#[test]
fn report_is_the_public_vector_and_failure_list_one_to_one() {
    use bss_orders::domain::gate::MAPPING_VERSION;
    use bss_orders_sdk::errors::RefusalDiagnostics;
    let a = assess(&mixed()).unwrap();
    let id = Uuid::from_u128(900);
    let report = a.report(id);
    assert_eq!(report.assessment_id, id);
    assert_eq!(report.mapping_version, MAPPING_VERSION);
    let outcomes = a.vector.outcomes();
    assert_eq!(report.outcomes.len(), outcomes.len());
    for (g, o) in report.outcomes.iter().zip(outcomes) {
        // The stored tokens, identity verbatim.
        assert_eq!(g.predicate, o.key.predicate.token());
        assert_eq!(g.verdict, o.verdict.token());
        assert_eq!(g.applicability, o.applicability.token());
        assert_eq!(g.reason, o.reason);
        assert_eq!(g.line_id, o.key.line_id);
        assert_eq!(g.item_id, o.key.item_id);
        assert_eq!(g.selection, o.key.selection);
    }
    assert!(report.failures.len() >= 6);
    assert_eq!(report.failures.len(), a.failures.len());
    for (g, f) in report.failures.iter().zip(&a.failures) {
        assert_eq!(g.line_id, f.line_id);
        assert_eq!(g.reason, f.reason);
        assert_eq!(g.predicate, f.predicate.token());
    }
    // The member carries the report unchanged through the SDK wire shape.
    let diagnostics = RefusalDiagnostics {
        assessment: Some(Box::new(report.clone())),
        ..RefusalDiagnostics::default()
    };
    let back: RefusalDiagnostics =
        serde_json::from_value(serde_json::to_value(&diagnostics).unwrap()).unwrap();
    assert_eq!(back.assessment, Some(Box::new(report)));
}

// ---- inputs follow the mapping's unavailable rows (D-227 item 1) ----

/// A `resolve` answer for another revision is malformed: the mapping makes the line's resolve
/// rows unavailable, and the engine inputs that port feeds are unresolvable, never resolved.
#[test]
fn malformed_catalog_answer_marks_the_fed_inputs_unresolvable() {
    let l1 = line_id(1);
    let mut i = base(Operation::Submit);
    let other = Uuid::from_u128(99);
    if let Ok(rb) = &mut i.catalog.get_mut(&l1).unwrap().resolved {
        Arc::make_mut(rb).revision_id = other;
    }
    let a = assess(&i).unwrap();
    for o in rows(&a.vector, PredicateId::CatalogItemMembership)
        .into_iter()
        .filter(|o| o.key.line_id == Some(l1))
    {
        assert_eq!(vr(o), unevaluable(Reason::CatalogPredicatesUnavailable));
    }
    assert_eq!(
        vr(row(&a.vector, PredicateId::PinComposition, Some(l1))),
        unevaluable(Reason::CatalogPinCompositionUnavailable)
    );
    assert_eq!(
        a.inputs.get(InputPort::CatalogPredicates),
        Some(InputState::Unresolvable(
            Reason::CatalogPredicatesUnavailable
        ))
    );
    assert_eq!(
        a.inputs.get(InputPort::PinComposition),
        Some(InputState::Unresolvable(
            Reason::CatalogPinCompositionUnavailable
        ))
    );
    assert_eq!(
        a.inputs.get(InputPort::PricingRevision),
        Some(InputState::Resolved)
    );

    // A `current_revision` failure other than `PLAN_UNPUBLISHED` is malformed too.
    let mut i = base(Operation::Submit);
    i.catalog.get_mut(&l1).unwrap().current = Err(ReadFailure::RevisionAbsent);
    let a = assess(&i).unwrap();
    assert_eq!(
        vr(row(
            &a.vector,
            PredicateId::CatalogRevisionCurrent,
            Some(l1)
        )),
        unevaluable(Reason::PricingRevisionUnavailable)
    );
    assert_eq!(
        a.inputs.get(InputPort::PricingRevision),
        Some(InputState::Unresolvable(Reason::PricingRevisionUnavailable))
    );
}

// ---- D-215/D-228 payer withholding ----

/// D-230: with the D-189 assessment port Missing and a catalog port down, the primary of both
/// Submit and Preview is still predicate 1's definitive local refusal, never the 503 port reason.
fn assert_local_refusal_outranks_outages(i: &AssessmentInput, expected: Reason, ctx: &str) {
    let mut down = i.clone();
    down.assessment = PortAnswer::Unavailable(Unavailable::Missing);
    down.catalog.remove(&line_id(2));
    let d = assess(&down).unwrap();
    assert!(
        d.vector
            .failures()
            .any(|o| o.reason == Some(Reason::CatalogPredicatesUnavailable)),
        "{ctx}: the catalog rows are unevaluable"
    );
    for op in [Operation::Submit, Operation::Preview] {
        assert_eq!(
            d.vector.primary(op).expect("blocked").reason,
            Some(expected),
            "{ctx} primary({op:?}) with providers down"
        );
    }
    // D-230 engine presentation: the definitive local primary presents every composite-gate
    // input `Resolved`, the down catalog ports' `CatalogPredicates` and `PricingRevision` too.
    assert_every_gate_input_resolved(&d, ctx);
}

/// The eight composite-gate inputs, all `Resolved` (D-230 engine presentation).
fn assert_every_gate_input_resolved(a: &Assessment, ctx: &str) {
    for input in ALL_PORTS
        .into_iter()
        .filter(|p| *p != InputPort::AcceptanceRequirement)
    {
        assert_eq!(
            a.inputs.get(input),
            Some(InputState::Resolved),
            "{ctx} {input:?}"
        );
    }
}

/// D-228 item 6: every payer-dependent row, the composed pin rows included (their billing terms
/// are withheld), carries predicate 1's `reason` as `required`; no failure carries the 503 pin
/// reason; the five withheld inputs, `PinComposition` and `Evaluation` included, are `Resolved`
/// (the composite gate resolves them by withholding; D-113 `Precluded` is for inputs an
/// earlier-registered guard makes moot).
fn assert_payer_withheld(a: &Assessment, reason: Reason, ctx: &str) {
    let withheld = unevaluable(reason);
    assert_eq!(
        vr(row(&a.vector, PredicateId::ContractActive, None)),
        withheld,
        "{ctx}"
    );
    for l in [line_id(1), line_id(2)] {
        for p in [
            PredicateId::MarketConsistency,
            PredicateId::OverlapUniqueness,
            PredicateId::InFlightPerKey,
            PredicateId::PinComposition,
        ] {
            assert_eq!(vr(row(&a.vector, p, Some(l))), withheld, "{ctx} {p:?}");
        }
        // Withheld terms are predicate 1's reason, required even on a Preview.
        assert_eq!(
            row(&a.vector, PredicateId::PinComposition, Some(l)).applicability,
            Applicability::Required,
            "{ctx}"
        );
    }
    assert!(
        a.failures
            .iter()
            .all(|f| f.reason != Reason::CatalogPinCompositionUnavailable),
        "{ctx}: withheld terms are not a port outage: {:#?}",
        a.failures
    );
    for input in [
        InputPort::ContractResolution,
        InputPort::OverlapKey,
        InputPort::Occupancy,
        InputPort::PinComposition,
        InputPort::Evaluation,
    ] {
        assert_eq!(
            a.inputs.get(input),
            Some(InputState::Resolved),
            "{ctx} {input:?}"
        );
    }
}

/// With the payer relationship unconfirmed no payer fact is read or disclosed: the
/// payer-dependent rows carry `payer-relationship-unconfirmed`, whatever the contract, overlap
/// and occupancy ports answered, and their inputs are `Resolved` by the gate's withholding
/// (D-228 item 6).
#[test]
fn unconfirmed_payer_relationship_withholds_payer_dependent_rows() {
    let disclosing = |r: Reason| {
        let t = r.token();
        t.starts_with("contract-") || t.starts_with("overlap-") || t.starts_with("market-")
    };
    let c = Uuid::from_u128(70);
    for op in [Operation::Submit, Operation::Preview] {
        for (seller, resource) in [(false, true), (true, false), (false, false)] {
            // Ports that would fail or be unavailable if they were consulted.
            for overlap_down in [true, false] {
                let contracts = if overlap_down {
                    unavailable()
                } else {
                    PortAnswer::Answered(BTreeMap::new())
                };
                let mut i = base(op);
                i.identity = PortAnswer::Answered(PayerProfile {
                    in_relationship_with_seller: seller,
                    in_relationship_with_resource: resource,
                    ..profile()
                });
                i.contract_id = Some(c);
                i.lines[1].contract_ref = Some(c);
                i.contracts = contracts;
                if overlap_down {
                    i.overlap_keys = unavailable();
                    i.occupancy = unavailable();
                    // The orchestrator does not ask billing terms (D-215 item 2): Missing. The
                    // policy is still asked; its outage must not decide the pin rows either.
                    i.billing_terms = PortAnswer::Unavailable(Unavailable::Missing);
                    i.acceptance_policy = unavailable();
                } else {
                    i.occupancy = PortAnswer::Answered(vec![occupancy(
                        &key_of(line_id(1)),
                        5,
                        MaxConcurrentActive::Limit(1),
                    )]);
                }
                let a = assess(&i).unwrap();
                let ctx = format!("{op:?} seller={seller} resource={resource}");
                let primary = a.primary.as_ref().expect("blocked");
                assert_eq!(
                    primary.reason,
                    Some(Reason::PayerRelationshipUnconfirmed),
                    "{ctx}"
                );
                // D-230: the refusal outranks catalog and provider unavailability too.
                assert_local_refusal_outranks_outages(
                    &i,
                    Reason::PayerRelationshipUnconfirmed,
                    &ctx,
                );
                assert!(
                    a.vector
                        .outcomes()
                        .iter()
                        .filter_map(|o| o.reason)
                        .all(|r| !disclosing(r)),
                    "{ctx}: {:#?}",
                    a.vector.failures().collect::<Vec<_>>()
                );
                assert_payer_withheld(&a, Reason::PayerRelationshipUnconfirmed, &ctx);
                // One failure entry per order/line for the reason.
                let unconfirmed: Vec<_> = a
                    .failures
                    .iter()
                    .filter(|f| f.reason == Reason::PayerRelationshipUnconfirmed)
                    .map(|f| f.line_id)
                    .collect();
                assert_eq!(unconfirmed, [None, Some(line_id(1)), Some(line_id(2))]);
            }
        }
    }

    // Confirmed relationship: unchanged — the contract port decides its row and input.
    let mut i = base(Operation::Submit);
    i.lines[1].contract_ref = Some(c);
    i.contracts = unavailable();
    let a = assess(&i).unwrap();
    assert_eq!(
        vr(row(&a.vector, PredicateId::ContractActive, None)),
        unevaluable(Reason::ContractResolutionUnavailable)
    );
    assert_eq!(
        a.inputs.get(InputPort::ContractResolution),
        Some(InputState::Unresolvable(
            Reason::ContractResolutionUnavailable
        ))
    );
    assert_eq!(
        a.inputs.get(InputPort::OverlapKey),
        Some(InputState::Resolved)
    );
}

/// Regression guard: confirmed, with billing terms unavailable, the pin rows and the
/// `PinComposition` input keep the port's reason (only withheld terms carry predicate 1's).
#[test]
fn confirmed_payer_with_terms_unavailable_keeps_the_pin_port_reason() {
    let mut i = base(Operation::Submit);
    i.billing_terms = unavailable();
    let a = assess(&i).unwrap();
    for l in [line_id(1), line_id(2)] {
        assert_eq!(
            vr(row(&a.vector, PredicateId::PinComposition, Some(l))),
            unevaluable(Reason::CatalogPinCompositionUnavailable)
        );
    }
    assert!(
        a.failures
            .iter()
            .any(|f| f.reason == Reason::CatalogPinCompositionUnavailable)
    );
    assert_eq!(
        a.inputs.get(InputPort::PinComposition),
        Some(InputState::Unresolvable(
            Reason::CatalogPinCompositionUnavailable
        ))
    );
}

/// With the axes also invalid, predicate 1's reason is `axis-invalid`: the withheld rows and the
/// primary carry it, and payer facts stay undisclosed (D-228 item 6).
#[test]
fn unconfirmed_relationship_with_invalid_axes_withholds_under_axis_invalid() {
    let disclosing = |r: Reason| {
        let t = r.token();
        t.starts_with("contract-") || t.starts_with("overlap-") || t.starts_with("market-")
    };
    let c = Uuid::from_u128(70);
    for op in [Operation::Submit, Operation::Preview] {
        let mut i = base(op);
        i.identity = PortAnswer::Answered(PayerProfile {
            axes_valid: false,
            in_relationship_with_seller: false,
            ..profile()
        });
        i.lines[1].contract_ref = Some(c);
        let a = assess(&i).unwrap();
        let ctx = format!("{op:?} axes invalid");
        assert_eq!(
            a.primary.as_ref().expect("blocked").reason,
            Some(Reason::AxisInvalid),
            "{ctx}"
        );
        // D-230: `axis-invalid` outranks catalog and provider unavailability.
        assert_local_refusal_outranks_outages(&i, Reason::AxisInvalid, &ctx);
        assert!(
            a.vector
                .outcomes()
                .iter()
                .filter_map(|o| o.reason)
                .all(|r| !disclosing(r) && r != Reason::PayerRelationshipUnconfirmed),
            "{ctx}"
        );
        assert_payer_withheld(&a, Reason::AxisInvalid, &ctx);
        // Terms and evaluation not asked (D-215 item 2): still predicate 1's reason, never the
        // port's, and the unasked evaluation is `Resolved`, not `Unresolvable`.
        let mut unasked = i.clone();
        unasked.billing_terms = PortAnswer::Unavailable(Unavailable::Missing);
        unasked.evaluation = PortAnswer::Unavailable(Unavailable::Missing);
        let a = assess(&unasked).unwrap();
        assert_payer_withheld(&a, Reason::AxisInvalid, &ctx);
    }
}

/// D-230: with no definitive local refusal, provider outages keep the D-195 primary: the first
/// unevaluable row, as its port's 503 reason.
#[test]
fn provider_outages_alone_keep_the_unavailable_primary() {
    for op in [Operation::Submit, Operation::Preview] {
        let mut i = base(op);
        i.assessment = PortAnswer::Unavailable(Unavailable::Missing);
        i.catalog.remove(&line_id(2));
        let a = assess(&i).unwrap();
        let primary = a.primary.as_ref().expect("blocked");
        assert_eq!(primary.verdict, Verdict::Unevaluable, "{op:?}");
        let first = a
            .vector
            .failures()
            .find(|o| o.verdict == Verdict::Unevaluable)
            .unwrap();
        assert_eq!(primary, first, "{op:?}");
        assert!(
            matches!(
                primary.reason,
                Some(Reason::CatalogPredicatesUnavailable | Reason::PricingRevisionUnavailable)
            ),
            "{op:?}: {:?}",
            primary.reason
        );
    }
}

/// Regression guard for the D-230 engine presentation: with the payer confirmed and no local
/// refusal, a down catalog port keeps `CatalogPredicates` `Unresolvable` (D-113 unavailable-first
/// at the engine stays for provider-backed rows).
#[test]
fn confirmed_payer_catalog_outage_keeps_catalog_predicates_unresolvable() {
    for op in [Operation::Submit, Operation::Preview] {
        let mut i = base(op);
        i.assessment = PortAnswer::Unavailable(Unavailable::Missing);
        let a = assess(&i).unwrap();
        assert_eq!(
            a.inputs.get(InputPort::CatalogPredicates),
            Some(InputState::Unresolvable(
                Reason::CatalogPredicatesUnavailable
            )),
            "{op:?}"
        );
        let mut i = base(op);
        i.catalog.remove(&line_id(2));
        let a = assess(&i).unwrap();
        assert_eq!(
            a.inputs.get(InputPort::CatalogPredicates),
            Some(InputState::Unresolvable(
                Reason::CatalogPredicatesUnavailable
            )),
            "{op:?}"
        );
    }
}

// ---- engine level: the assessment's inputs settle through `transition::decide` ----

static TABLE: std::sync::LazyLock<StateTable> =
    std::sync::LazyLock::new(|| StateTable::registered().unwrap());

/// A draft order at version 2, draft revision 4.
fn draft_facts() -> AggregateFacts {
    let t0 = OffsetDateTime::from_unix_timestamp(1_790_000_000).unwrap();
    AggregateFacts {
        order_id: Uuid::from_u128(1),
        state: OrderState::Draft,
        state_entered_at: t0,
        current_version: 2,
        version_allocation_high_water: 2,
        draft_revision: 4,
        pre_hold_state: None,
        resume_count: 0,
        amendment_count: 0,
        fulfillment_control_generation: 0,
        fulfillment_control_pending: None,
        spawn_signal_at: None,
        authorization_failure_tolerated_at: None,
        compensation_evidence: None,
        resource_tenant_id: Uuid::from_u128(10),
        seller_tenant_id: Uuid::from_u128(20),
        payer_tenant_id: Uuid::from_u128(30),
        category: "gts.cf.bss.orders.category.v1~cf.bss.orders.new_sale.v1".into(),
        contract_id: None,
    }
}

fn pass(_: &GuardSubject<'_>) -> GuardVerdict {
    GuardVerdict::Pass
}

/// Submit (row 4) with `a.inputs`: every row-4 guard bound to pass except the composite gate,
/// bound to `Fail(primary.reason)` (passing when nothing blocks), as S3-12 binds it.
fn settle(a: &Assessment) -> Result<Decision<'static>, DecisionError> {
    let table: &'static StateTable = &TABLE;
    let registry = GuardRegistry::registered(table).unwrap();
    let primary = a.primary.as_ref().and_then(|o| o.reason);
    let mut bindings = GuardBindings::new();
    for g in registry.row_guards(RowId::of(4)) {
        bindings = match (g.name == GATE_COMPOSITE, primary) {
            (true, Some(reason)) => bindings.bind(g.name, move |_: &GuardSubject<'_>| {
                GuardVerdict::Fail(reason)
            }),
            _ => bindings.bind(g.name, pass),
        };
    }
    decide(
        table,
        &registry,
        Trigger::Submit,
        &draft_facts(),
        2,
        DraftRevisions::Draft {
            client: Some(4),
            prepared: 4,
        },
        &bindings,
        &a.inputs,
        OffsetDateTime::from_unix_timestamp(1_790_000_100).unwrap(),
    )
}

/// The orchestrated shape of an unconfirmed basket: billing terms and the evaluation not asked
/// (`Missing`, D-215 item 2), contracts, overlap keys and occupancy not asked (`Missing`), and a
/// catalog port down.
fn orchestrated(mut i: AssessmentInput) -> AssessmentInput {
    i.billing_terms = PortAnswer::Unavailable(Unavailable::Missing);
    i.evaluation = PortAnswer::Unavailable(Unavailable::Missing);
    i.contracts = PortAnswer::Unavailable(Unavailable::Missing);
    i.overlap_keys = PortAnswer::Unavailable(Unavailable::Missing);
    i.occupancy = PortAnswer::Unavailable(Unavailable::Missing);
    i.catalog.remove(&line_id(2));
    i
}

fn assert_gate_refusal(i: &AssessmentInput, reason: Reason, ctx: &str) {
    let a = assess(i).unwrap();
    assert_eq!(
        settle(&a),
        Ok(Decision::Guard {
            guard: GATE_COMPOSITE,
            reason,
            assessment: true,
        }),
        "{ctx}: {:?}",
        a.inputs
    );
}

/// D-228 item 6 + D-230 at the engine: an unconfirmed payer settles the gate's 400
/// `payer-relationship-unconfirmed`, never `PrecludedReached` (500) nor `Unevaluable` (503),
/// with every port answered and in the orchestrated shape.
#[test]
fn engine_settles_unconfirmed_payer_as_the_gate_refusal() {
    let mut i = base(Operation::Submit);
    i.identity = PortAnswer::Answered(PayerProfile {
        in_relationship_with_seller: false,
        ..profile()
    });
    assert_gate_refusal(&i, Reason::PayerRelationshipUnconfirmed, "answered");
    assert_gate_refusal(
        &orchestrated(i),
        Reason::PayerRelationshipUnconfirmed,
        "orchestrated",
    );
}

/// The same with the axes also invalid: the gate's 400 `axis-invalid`.
#[test]
fn engine_settles_invalid_axes_as_the_gate_refusal() {
    let mut i = base(Operation::Submit);
    i.identity = PortAnswer::Answered(PayerProfile {
        axes_valid: false,
        in_relationship_with_resource: false,
        ..profile()
    });
    assert_gate_refusal(&i, Reason::AxisInvalid, "answered");
    assert_gate_refusal(&orchestrated(i), Reason::AxisInvalid, "orchestrated");
}

/// D-113 kept: confirmed payer, only a catalog outage, no local refusal: the engine settles the
/// gate unevaluable with the catalog input's reason (503).
#[test]
fn engine_settles_a_confirmed_catalog_outage_as_unevaluable() {
    let mut i = base(Operation::Submit);
    i.assessment = PortAnswer::Unavailable(Unavailable::Missing);
    let a = assess(&i).unwrap();
    assert_eq!(
        settle(&a),
        Ok(Decision::Unevaluable {
            guard: GATE_COMPOSITE,
            reason: Reason::CatalogPredicatesUnavailable,
            assessment: true,
        })
    );
}

#[test]
fn a_held_key_fails_predicate_nine_even_when_occupancy_is_unavailable() {
    let mut i = base(Operation::Submit);
    let key = i.overlap_keys.answered().unwrap()[0]
        .overlap_scope_key
        .clone();
    let line = i.overlap_keys.answered().unwrap()[0].line_id;
    i.inflight_same_tuple.insert(key);
    for missing_occupancy in [false, true] {
        if missing_occupancy {
            i.occupancy = unavailable();
        }
        let a = assess(&i).unwrap();
        assert_eq!(
            vr(row(&a.vector, PredicateId::InFlightPerKey, Some(line))),
            (Verdict::Failed, Some(Reason::OrderInFlightForKey))
        );
    }
}

#[test]
fn a_claim_collision_replaces_only_blocked_lines_in_the_vector() {
    let i = base(Operation::Submit);
    let a = assess(&i).unwrap();
    let blocked = line_id(1);
    let v = a
        .vector
        .replace_inflight(&[blocked].into_iter().collect())
        .unwrap();
    assert_eq!(
        vr(row(&v, PredicateId::InFlightPerKey, Some(blocked))),
        (Verdict::Failed, Some(Reason::OrderInFlightForKey))
    );
    for original in a.vector.outcomes() {
        if original.key.predicate != PredicateId::InFlightPerKey
            || original.key.line_id != Some(blocked)
        {
            assert!(v.outcomes().contains(original));
        }
    }
    assert_eq!(v.outcomes().len(), a.vector.outcomes().len());
    let report = v.report(Uuid::nil());
    let failure = report
        .failures
        .iter()
        .find(|f| f.predicate == "orders.in-flight-per-key")
        .unwrap();
    assert_eq!(failure.reason, Reason::OrderInFlightForKey);
    assert_eq!(failure.line_id, Some(blocked));
}

#[test]
fn engine_settles_a_held_key_as_the_composite_gate_refusal() {
    let mut i = base(Operation::Submit);
    i.inflight_same_tuple.insert(key_of(line_id(1)));
    assert_gate_refusal(&i, Reason::OrderInFlightForKey, "same-tuple holder");
}

#[test]
fn non_recurring_lines_pass_overlap_rows_even_without_overlap_ports() {
    for kind in [ChargeKind::OneTime, ChargeKind::Usage] {
        let mut i = input_for(
            Operation::Submit,
            vec![basket_line(1, vec![sel(ITEM, None, "1")])],
        );
        let mut c = catalog_answer(vec![cell(ITEM, None, true)]);
        Arc::make_mut(c.resolved.as_mut().unwrap()).cells[0]
            .binding
            .as_mut()
            .unwrap()
            .kind = kind.clone();
        i.catalog.insert(line_id(1), c);
        i.overlap_keys = unavailable();
        i.occupancy = unavailable();
        let a = assess(&i).unwrap();
        for predicate in [PredicateId::OverlapUniqueness, PredicateId::InFlightPerKey] {
            let outcome = row(&a.vector, predicate, Some(line_id(1)));
            assert_eq!(vr(outcome), (Verdict::Passed, None), "{kind:?}");
            assert_eq!(
                outcome
                    .upstream_detail
                    .as_ref()
                    .unwrap()
                    .upstream_code
                    .as_deref(),
                Some("not-applicable-no-paid-recurring-sku")
            );
        }
        assert!(a.primary.is_none(), "{kind:?}: {:?}", a.primary);
        for input in [InputPort::OverlapKey, InputPort::Occupancy] {
            assert_eq!(a.inputs.get(input), Some(InputState::Resolved));
        }
    }
}

/// A keyless line never consumes recurring cardinality; an extraneous key for it is an unasked
/// entry, so the answer is rejected before reads and no row 7 or 9 passes (R8).
#[test]
fn keyless_lines_do_not_consume_recurring_cardinality_and_an_extraneous_key_rejects_the_answer() {
    let mut i = base(Operation::Submit);
    let mut c = good_catalog();
    for cell in &mut Arc::make_mut(c.resolved.as_mut().unwrap()).cells {
        cell.binding.as_mut().unwrap().kind = ChargeKind::OneTime;
    }
    i.catalog.insert(line_id(1), c);
    let k = key_of(line_id(2));
    let unique_l2 = |i: &AssessmentInput| {
        vr(row(
            &assess(i).unwrap().vector,
            PredicateId::OverlapUniqueness,
            Some(line_id(2)),
        ))
    };
    i.overlap_keys = PortAnswer::Answered(overlap_keys(line_id(2), &k));
    assert_eq!(unique_l2(&i), (Verdict::Passed, None));
    i.overlap_keys =
        PortAnswer::Answered([overlap_keys(line_id(1), &k), overlap_keys(line_id(2), &k)].concat());
    i.occupancy = PortAnswer::Answered(Vec::new());
    assert_eq!(
        unique_l2(&i),
        (
            Verdict::Unevaluable,
            Some(Reason::OverlapPresenceUnevaluable)
        )
    );
    assert_eq!(
        vr(row(
            &assess(&i).unwrap().vector,
            PredicateId::InFlightPerKey,
            Some(line_id(2)),
        )),
        unevaluable(Reason::OverlapKeyUnresolvable)
    );
}

#[test]
fn assessment_profile_mismatch_never_admits_a_passed_row() {
    let mut input = base(Operation::Submit);
    let mut answer = passing_assessment(&two_lines());
    answer.profile = "another-profile".into();
    input.assessment = PortAnswer::Answered(answer);
    let a = assess(&input).unwrap();
    assert_eq!(
        vr(row(
            &a.vector,
            PredicateId::CatalogRevisionAvailable,
            Some(line_id(1))
        )),
        unevaluable(Reason::CatalogPredicatesUnavailable)
    );
    assert_catalog_unavailable_settlement(&a);
}

fn assert_catalog_unavailable_settlement(assessment: &Assessment) {
    assert_eq!(
        assessment.inputs.get(InputPort::CatalogPredicates),
        Some(InputState::Unresolvable(
            Reason::CatalogPredicatesUnavailable
        ))
    );
    assert_eq!(
        settle(assessment),
        Ok(Decision::Unevaluable {
            guard: GATE_COMPOSITE,
            reason: Reason::CatalogPredicatesUnavailable,
            assessment: true,
        })
    );
}

#[test]
fn malformed_assessment_coverage_settles_as_an_unavailable_input() {
    for defect in ["missing-row", "unknown-predicate", "duplicate-row"] {
        let mut input = base(Operation::Submit);
        let mut answer = passing_assessment(&two_lines());
        match defect {
            "missing-row" => {
                answer.results.remove(0);
            }
            "unknown-predicate" => answer.results[0].result.predicate = "unknown-predicate".into(),
            "duplicate-row" => answer.results.push(answer.results[0].clone()),
            _ => unreachable!(),
        }
        input.assessment = PortAnswer::Answered(answer);
        let assessment = assess(&input).unwrap();
        assert_catalog_unavailable_settlement(&assessment);
    }
}

#[test]
fn malformed_assessment_preserves_definitive_refusal_and_payer_withholding() {
    for reason in [
        Reason::AxisInvalid,
        Reason::PayerRelationshipUnconfirmed,
        Reason::QuantityBelowFloor,
    ] {
        let mut input = base(Operation::Submit);
        let mut answer = passing_assessment(&two_lines());
        answer.profile = "another-profile".into();
        input.assessment = PortAnswer::Answered(answer);
        match reason {
            Reason::AxisInvalid => {
                input.identity = PortAnswer::Answered(PayerProfile {
                    axes_valid: false,
                    ..profile()
                });
            }
            Reason::PayerRelationshipUnconfirmed => {
                input.identity = PortAnswer::Answered(PayerProfile {
                    in_relationship_with_seller: false,
                    ..profile()
                });
            }
            Reason::QuantityBelowFloor => input.lines[0].catalog.items[0].quantity = "0".into(),
            _ => unreachable!(),
        }
        assert_gate_refusal(&input, reason, "malformed assessment");
        if reason != Reason::QuantityBelowFloor {
            assert_gate_refusal(
                &orchestrated(input),
                reason,
                "malformed assessment with payer facts withheld",
            );
        }
    }
}

#[test]
fn unavailable_evaluation_refuses_without_changing_the_predicate_vector() {
    let mut input = base(Operation::Submit);
    let baseline = assess(&input).unwrap();
    input.evaluation = unavailable();
    let assessment = assess(&input).unwrap();
    assert!(baseline.failures.is_empty());
    assert!(assessment.failures.is_empty());
    assert!(assessment.primary.is_none());
    let run_id = Uuid::from_u128(99);
    assert_eq!(assessment.report(run_id), baseline.report(run_id));
    assert_eq!(assessment.vector, baseline.vector);
    assert_eq!(
        assessment.inputs.get(InputPort::Evaluation),
        Some(InputState::Unresolvable(Reason::EvaluationUnavailable))
    );
    assert_eq!(
        settle(&assessment),
        Ok(Decision::Unevaluable {
            guard: GATE_COMPOSITE,
            reason: Reason::EvaluationUnavailable,
            assessment: true,
        })
    );
}

#[test]
fn count_scope_controls_other_resource_addend_but_keeps_predicate_nine() {
    for (scope, limit, expected) in [
        (
            "per_payer",
            Some(1),
            failed(Reason::OverlapCardinalityExceeded),
        ),
        ("full_tuple", Some(1), PASS),
        (
            "per_payer",
            Some(0),
            failed(Reason::OverlapCardinalityExceeded),
        ),
        (
            "full_tuple",
            Some(0),
            failed(Reason::OverlapCardinalityExceeded),
        ),
        ("per_payer", None, PASS),
        ("full_tuple", None, PASS),
    ] {
        let mut input = base(Operation::Submit);
        let line = line_id(1);
        let key = key_of(line);
        let answer: Occupancy = serde_json::from_value(serde_json::json!({
            "active_count": 0, "max_concurrent_active": limit,
            "provenance": "catalog", "count_scope": scope
        }))
        .unwrap();
        input.occupancy = PortAnswer::Answered(vec![(
            occupancy(&key, 0, MaxConcurrentActive::Limit(1)).0,
            answer.into(),
        )]);
        input.inflight_other_orders.insert(key.clone(), 1);
        input.inflight_same_tuple.insert(key);
        let result = assess(&input).unwrap();
        assert_eq!(
            vr(row(
                &result.vector,
                PredicateId::OverlapUniqueness,
                Some(line)
            )),
            expected,
            "{scope} {limit:?}"
        );
        assert_eq!(
            vr(row(&result.vector, PredicateId::InFlightPerKey, Some(line))),
            failed(Reason::OrderInFlightForKey)
        );
    }
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "json! yields owned values at every call site"
)]
fn protected_observation(
    key: &str,
    suspended: u32,
    reserved: u32,
    limit: serde_json::Value,
) -> bss_orders::domain::gate::answers::OccupancyObservation {
    serde_json::from_value(serde_json::json!({
        "profile":"protected_full_tuple_v1", "demand_unit":"line_scope_v1",
        "observation_revision":"snapshot-1",
        "scope":{"payer_tenant_id":TENANT,"resource_tenant_id":TENANT,"overlap_scope_key":key},
        "policy":{"source":"catalog","reference":"catalog-policy","version":"1"},
        "active_count":0,"retained_suspended_count":suspended,"order_reserved_count":reserved,
        "max_concurrent_active":limit,"own_membership":null
    }))
    .unwrap()
}

#[test]
fn protected_retained_and_reserved_rights_block_new_demand_with_no_active_rights() {
    for (suspended, reserved) in [(1, 0), (0, 1)] {
        let mut input = base(Operation::Submit);
        input.occupancy_axes = Some(occupancy_axes());
        let PortAnswer::Answered(rows) = &mut input.occupancy else {
            panic!()
        };
        for (q, o) in rows {
            *o = protected_observation(
                &q.overlap_scope_key,
                suspended,
                reserved,
                serde_json::json!(1),
            );
        }
        let result = assess(&input).unwrap();
        assert!(
            result
                .failures
                .iter()
                .any(|f| f.reason == Reason::OverlapCardinalityExceeded)
        );
        assert_eq!(
            result.inputs.get(InputPort::Occupancy),
            Some(InputState::Resolved)
        );
        assert_gate_refusal(
            &input,
            Reason::OverlapCardinalityExceeded,
            "valid protected capacity excess",
        );
    }
}

#[test]
fn protected_full_tuple_ignores_legacy_cross_resource_addend_and_keeps_p9() {
    let mut input = input_for(Operation::Submit, vec![two_lines().remove(1)]);
    input.occupancy_axes = Some(occupancy_axes());
    let key = key_of(line_id(2));
    let q = occupancy(&key, 0, MaxConcurrentActive::Limit(1)).0;
    input.occupancy = PortAnswer::Answered(vec![(
        q,
        protected_observation(&key, 0, 0, serde_json::json!(1)),
    )]);
    input.inflight_other_orders.insert(key.clone(), 99);
    let result = assess(&input).unwrap();
    assert_eq!(
        vr(row(
            &result.vector,
            PredicateId::OverlapUniqueness,
            Some(line_id(2))
        )),
        PASS
    );
    input.inflight_same_tuple.insert(key);
    let result = assess(&input).unwrap();
    assert_eq!(
        vr(row(
            &result.vector,
            PredicateId::InFlightPerKey,
            Some(line_id(2))
        )),
        failed(Reason::OrderInFlightForKey)
    );
}

#[test]
fn protected_occupancy_query_carries_payer_as_payer_and_resource_as_resource() {
    let payer = Uuid::from_u128(51);
    let resource = Uuid::from_u128(52);
    let mut input = input_for(Operation::Submit, vec![two_lines().remove(1)]);
    input.occupancy_axes = Some(TenantAxes {
        resource,
        payer,
        seller: TENANT,
    });
    let key = key_of(line_id(2));
    let query = OccupancyQuery {
        own_context: None,
        payer_tenant_id: payer,
        resource_tenant_id: Some(resource),
        overlap_scope_key: key.clone(),
    };
    let observation: OccupancyObservation = serde_json::from_value(serde_json::json!({
        "profile":"protected_full_tuple_v1", "demand_unit":"line_scope_v1",
        "observation_revision":"snapshot-1",
        "scope":{"payer_tenant_id":payer,"resource_tenant_id":resource,"overlap_scope_key":key},
        "policy":{"source":"catalog","reference":"catalog-policy","version":"1"},
        "active_count":0,"retained_suspended_count":0,"order_reserved_count":0,
        "max_concurrent_active":1,"own_membership":null
    }))
    .unwrap();
    input.occupancy = PortAnswer::Answered(vec![(query, observation)]);
    let result = assess(&input).unwrap();
    assert_eq!(
        vr(row(
            &result.vector,
            PredicateId::OverlapUniqueness,
            Some(line_id(2))
        )),
        PASS,
        "a row answering the (payer, resource) query validates; a swapped axis would not"
    );
    assert_eq!(
        result.inputs.get(InputPort::Occupancy),
        Some(InputState::Resolved)
    );
}

#[test]
fn protected_assessment_validates_all_scopes_and_membership_before_unlimited_capacity() {
    let mut input = input_for(Operation::Submit, vec![two_lines().remove(1)]);
    input.occupancy_axes = Some(occupancy_axes());
    let first = key_of(line_id(2));
    let PortAnswer::Answered(keys) = &mut input.overlap_keys else {
        panic!()
    };
    keys[1].overlap_scope_key = "second".into();
    keys[1].item_key.as_mut().unwrap().overlap_scope_key = "second".into();
    input.occupancy = PortAnswer::Answered(vec![
        (
            occupancy(&first, 0, MaxConcurrentActive::Unlimited).0,
            protected_observation(&first, 0, 0, serde_json::Value::Null),
        ),
        (
            occupancy("second", 0, MaxConcurrentActive::Limit(1)).0,
            protected_observation("second", 0, 1, serde_json::json!(1)),
        ),
    ]);
    let result = assess(&input).unwrap();
    assert_eq!(
        vr(row(
            &result.vector,
            PredicateId::OverlapUniqueness,
            Some(line_id(2))
        )),
        failed(Reason::OverlapCardinalityExceeded)
    );
    let PortAnswer::Answered(rows) = &mut input.occupancy else {
        panic!()
    };
    let OccupancyObservation::ProtectedFullTupleV1(o) = &mut rows[1].1 else {
        panic!()
    };
    o.max_concurrent_active = MaxConcurrentActive::Unlimited;
    o.scope.resource_tenant_id = Uuid::from_u128(999);
    let result = assess(&input).unwrap();
    assert_eq!(
        vr(row(
            &result.vector,
            PredicateId::OverlapUniqueness,
            Some(line_id(2))
        )),
        unevaluable(Reason::OverlapPresenceUnevaluable)
    );
}

#[test]
fn protected_shared_scope_counts_distinct_lines_once_each() {
    let mut input = base(Operation::Preview);
    input.occupancy_axes = Some(occupancy_axes());
    let PortAnswer::Answered(keys) = &mut input.overlap_keys else {
        panic!()
    };
    for key in keys {
        key.overlap_scope_key = "shared".into();
        key.item_key.as_mut().unwrap().overlap_scope_key = "shared".into();
    }
    for (limit, want) in [(1, failed(Reason::OverlapCardinalityExceeded)), (2, PASS)] {
        input.occupancy = PortAnswer::Answered(vec![(
            occupancy("shared", 0, MaxConcurrentActive::Limit(limit)).0,
            protected_observation("shared", 0, 0, serde_json::json!(limit)),
        )]);
        let result = assess(&input).unwrap();
        for line in [line_id(1), line_id(2)] {
            assert_eq!(
                vr(row(
                    &result.vector,
                    PredicateId::OverlapUniqueness,
                    Some(line)
                )),
                want
            );
        }
    }
}

fn protected_input() -> AssessmentInput {
    let mut input = base(Operation::Submit);
    input.occupancy_axes = Some(occupancy_axes());
    let PortAnswer::Answered(rows) = &mut input.occupancy else {
        panic!()
    };
    for (q, o) in rows {
        *o = protected_observation(&q.overlap_scope_key, 0, 0, serde_json::Value::Null);
    }
    input
}

#[test]
fn invalid_protected_occupancy_resolves_engine_input_as_unavailable() {
    for defect in [
        "foreign-tuple",
        "unsolicited-membership",
        "missing-row",
        "duplicate-row",
        "mixed-revision",
        "mixed-profile",
    ] {
        let mut input = protected_input();
        let PortAnswer::Answered(rows) = &mut input.occupancy else {
            panic!()
        };
        match defect {
            "missing-row" => {
                rows.pop();
            }
            "duplicate-row" => rows.push(rows[0].clone()),
            "mixed-profile" => {
                rows[0].1 = occupancy(
                    &rows[0].0.overlap_scope_key,
                    0,
                    MaxConcurrentActive::Unlimited,
                )
                .1;
            }
            _ => {
                let OccupancyObservation::ProtectedFullTupleV1(o) = &mut rows[0].1 else {
                    panic!()
                };
                match defect {
                    "foreign-tuple" => o.scope.resource_tenant_id = Uuid::from_u128(999),
                    "mixed-revision" => o.observation_revision = "another-snapshot".into(),
                    "unsolicited-membership" => {
                        let mut wire = serde_json::to_value(&*o).unwrap();
                        wire["own_membership"] = serde_json::json!({
                            "complete":false,"context":{"order_id":TENANT,"version":1,"payer_tenant_id":TENANT,
                                "resource_tenant_id":TENANT,"line_ids":[]},"rights":[]
                        });
                        *o = serde_json::from_value(wire).unwrap();
                    }
                    _ => unreachable!(),
                }
            }
        }
        let assessment = assess(&input).unwrap();
        assert_eq!(
            assessment.inputs.get(InputPort::Occupancy),
            Some(InputState::Unresolvable(Reason::OverlapPresenceUnevaluable)),
            "{defect}"
        );
        assert_eq!(
            settle(&assessment),
            Ok(Decision::Unevaluable {
                guard: GATE_COMPOSITE,
                reason: Reason::OverlapPresenceUnevaluable,
                assessment: true
            }),
            "{defect}"
        );
        for line in [line_id(1), line_id(2)] {
            assert_eq!(
                vr(row(
                    &assessment.vector,
                    PredicateId::OverlapUniqueness,
                    Some(line)
                )),
                unevaluable(Reason::OverlapPresenceUnevaluable)
            );
        }
    }
}

#[test]
fn invalid_protected_occupancy_preserves_definitive_refusal_and_payer_withholding() {
    for reason in [
        Reason::AxisInvalid,
        Reason::PayerRelationshipUnconfirmed,
        Reason::QuantityBelowFloor,
    ] {
        let mut input = protected_input();
        let PortAnswer::Answered(rows) = &mut input.occupancy else {
            panic!()
        };
        let OccupancyObservation::ProtectedFullTupleV1(o) = &mut rows[0].1 else {
            panic!()
        };
        o.scope.resource_tenant_id = Uuid::from_u128(999);
        match reason {
            Reason::AxisInvalid => {
                input.identity = PortAnswer::Answered(PayerProfile {
                    axes_valid: false,
                    in_relationship_with_resource: false,
                    ..profile()
                });
            }
            Reason::PayerRelationshipUnconfirmed => {
                input.identity = PortAnswer::Answered(PayerProfile {
                    in_relationship_with_seller: false,
                    ..profile()
                });
            }
            Reason::QuantityBelowFloor => input.lines[0].catalog.items[0].quantity = "0".into(),
            _ => unreachable!(),
        }
        assert_gate_refusal(&input, reason, "invalid protected occupancy");
        if reason != Reason::QuantityBelowFloor {
            let assessment = assess(&input).unwrap();
            assert_eq!(
                vr(row(
                    &assessment.vector,
                    PredicateId::OverlapUniqueness,
                    Some(line_id(1))
                )),
                unevaluable(reason)
            );
        }
    }
}

/// C5 (n86): `Assessment::report` maps the assessment's own failure list 1:1; the dedup is the
/// one rule `Vector::failure_list`, which `assess` and `Vector::report` share.
#[test]
fn report_failures_equal_assessment_failures() {
    let a = assess(&mixed()).unwrap();
    assert_eq!(a.failures, a.vector.failure_list());
    let report = a.report(Uuid::from_u128(901));
    let mapped: Vec<_> = a
        .failures
        .iter()
        .map(|f| (f.line_id, f.reason, f.predicate.token()))
        .collect();
    let reported: Vec<_> = report
        .failures
        .iter()
        .map(|g| (g.line_id, g.reason, g.predicate.as_str()))
        .collect();
    assert_eq!(mapped, reported);
}

/// R2 (D-195, F03 §3.1): a producer reason the row's predicate does not own is unmapped, so the
/// row is invalid coverage (`catalog-predicates-unavailable`, 503), never a definitive local
/// refusal that hides an outage nor a `GuardReason` error (500); owned reasons still map.
#[test]
fn i_a_producer_reason_outside_the_predicates_own_is_invalid_coverage_d195() {
    let l2 = line_id(2);
    let with_market = |verdict: Verdict, token: &str, occupancy_down: bool| {
        let mut i = base(Operation::Submit);
        if occupancy_down {
            i.occupancy = unavailable();
        }
        let mut ans = passing_assessment(&two_lines());
        let mc = ans
            .results
            .iter_mut()
            .find(|r| {
                r.line_id == l2 && r.result.predicate == PredicateId::MarketConsistency.token()
            })
            .unwrap();
        mc.result.verdict = verdict;
        mc.result.reason = Some(token.into());
        i.assessment = PortAnswer::Answered(ans);
        assess(&i).unwrap()
    };
    let foreign = [
        (Verdict::Failed, "axis-invalid"),
        (Verdict::Failed, "request-invalid"),
        (Verdict::Failed, "catalog-predicate-failed"),
        (Verdict::Failed, "catalog-predicate-unevaluable"),
        (Verdict::Unevaluable, "axis-invalid"),
        (Verdict::Unevaluable, "market-inconsistent"),
    ];
    for (verdict, token) in foreign {
        for occupancy_down in [false, true] {
            let ctx = format!("{verdict:?} {token} occupancy_down={occupancy_down}");
            let a = with_market(verdict, token, occupancy_down);
            assert_eq!(
                vr(row(&a.vector, PredicateId::MarketConsistency, Some(l2))),
                unevaluable(Reason::CatalogPredicatesUnavailable),
                "{ctx}"
            );
            assert_eq!(
                a.inputs.get(InputPort::CatalogPredicates),
                Some(InputState::Unresolvable(
                    Reason::CatalogPredicatesUnavailable
                )),
                "{ctx}"
            );
            assert_eq!(
                settle(&a),
                Ok(Decision::Unevaluable {
                    guard: GATE_COMPOSITE,
                    reason: Reason::CatalogPredicatesUnavailable,
                    assessment: true,
                }),
                "{ctx}"
            );
        }
    }
    // The predicate's own reasons keep their meaning.
    let a = with_market(Verdict::Failed, "market-inconsistent", false);
    assert_eq!(
        vr(row(&a.vector, PredicateId::MarketConsistency, Some(l2))),
        failed(Reason::MarketInconsistent)
    );
    assert_eq!(
        settle(&a),
        Ok(Decision::Guard {
            guard: GATE_COMPOSITE,
            reason: Reason::MarketInconsistent,
            assessment: true,
        })
    );
    let a = with_market(Verdict::Unevaluable, "catalog-predicate-unevaluable", false);
    assert_eq!(
        vr(row(&a.vector, PredicateId::MarketConsistency, Some(l2))),
        unevaluable(Reason::CatalogPredicateUnevaluable)
    );
}

/// R8 (`SUBSCRIPTIONS_CONTRACT.md` exact coverage; D-179): a key answer rejected before reads (a
/// requested line without a validated collection, or a foreign entry) runs neither occupancy nor
/// the in-flight lookup, so rows 7 and 9 of every keyed line are unevaluable, never passed.
#[test]
fn c_a_rejected_key_answer_never_reports_predicate_nine_passed() {
    let (l1, l2) = (line_id(1), line_id(2));
    // What the orchestrator hands `assess` after rejecting the answer: nothing was read.
    let not_read = |mut i: AssessmentInput| {
        i.occupancy = PortAnswer::Answered(Vec::new());
        i.inflight_same_tuple.clear();
        i.inflight_other_orders.clear();
        i
    };
    let rejected = (
        unevaluable(Reason::OverlapPresenceUnevaluable),
        unevaluable(Reason::OverlapKeyUnresolvable),
        Some("overlap-key-answer-rejected".to_owned()),
    );
    let rows_of = |i: &AssessmentInput, l| {
        let a = assess(i).unwrap();
        let in_flight = row(&a.vector, PredicateId::InFlightPerKey, Some(l));
        (
            vr(row(&a.vector, PredicateId::OverlapUniqueness, Some(l))),
            vr(in_flight),
            in_flight
                .upstream_detail
                .as_ref()
                .and_then(|d| d.upstream_code.clone()),
        )
    };

    // l2 unanswered: l1's valid key is never read; l2 keeps its missing-key rows.
    let mut i = base(Operation::Submit);
    i.overlap_keys = PortAnswer::Answered(overlap_keys(l1, &key_of(l1)));
    let i = not_read(i);
    assert_eq!(rows_of(&i, l1), rejected);
    assert_eq!(
        rows_of(&i, l2),
        (
            failed(Reason::OverlapKeyUnresolvable),
            unevaluable(Reason::OverlapKeyUnresolvable),
            None
        )
    );

    // A foreign entry rejects the whole answer, as the orchestrator does.
    let mut i = base(Operation::Submit);
    let mut keys = i.overlap_keys.answered().unwrap().clone();
    keys.extend(overlap_keys(line_id(99), "foreign"));
    i.overlap_keys = PortAnswer::Answered(keys);
    let i = not_read(i);
    for l in [l1, l2] {
        assert_eq!(rows_of(&i, l), rejected, "{l}");
    }

    // An accepted answer still evaluates predicate 9.
    let mut i = base(Operation::Submit);
    i.inflight_same_tuple.insert(key_of(l1));
    assert_eq!(
        vr(row(
            &assess(&i).unwrap().vector,
            PredicateId::InFlightPerKey,
            Some(l1)
        )),
        failed(Reason::OrderInFlightForKey)
    );
}
