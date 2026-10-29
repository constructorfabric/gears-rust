#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once; `allow`, not `expect`, because not every item unwraps or expects"
)]
//! Submit phase A's frozen per-line Pricing requests (S3-12, D-188, D-192): every
//! `NewSaleQuery` member sourced from the basket, the run's answers and the frozen date basis;
//! the canonical `line_requests` value stored on the attempt; stable command keys; and digests
//! equal to the Pricing digest functions over the same inputs.
use bss_orders::domain::capture::line_cap_verdict;
use bss_orders::domain::contributions::VersionContribution;
use bss_orders::domain::dates::{
    DateBasis, DatePolicySnapshot, PLATFORM_DEFAULT_POLICY_ID, PolicyRow, SuppliedDates,
};
use bss_orders::domain::error::EngineError;
use bss_orders::domain::gate::answers::{
    AcceptancePolicy, Amount, Availability, BillingTermsRef, BillingTermsSnapshot,
    EvaluationChargeKind, EvaluationCycle, EvaluationTerm, ExactMoney, LineEvaluation, Market,
    OrderEvaluation, OrderEvaluationRequest, ResolvedTerms, Tcv, TcvBasis, TermsAnchor,
    TermsProvenance, TermsSnapshotSource, Totals, TotalsDeclaration,
};
use bss_orders::domain::gate::assess::BasketLine;
use bss_orders::domain::gate::commercial::{
    CONTRACT_DEFECT_REFUSALS, CommercialError, CommercialFacts, FrozenOrder, LineRequest,
    ORDER_MEMBER, RECORDED_REFUSALS, RECORDED_TERMS_REFUSALS, ReceiptResult, RefusalDisposition,
    SUBMIT_COMMAND, build_line_requests, command_key, contract_defect, decode_line_requests,
    decode_receipt_result, frozen_line_requests, line_requests_value, receipt_result_value,
    recorded_refusal, refusal_disposition, verify_evaluated_bindings, verify_evaluated_query,
    version_capacity, version_row_bytes,
};
use bss_orders::domain::gate::orchestrate::TenantAxes;
use bss_orders::domain::gate::predicates::PredicateId;
use bss_orders::domain::gate::pricing_map::{LineCatalogRequest, SelectedItem};
use bss_orders::domain::gate::totals::{
    AmountStatus, TotalRow, TotalScope, TotalsError, decimal_valid, total_rows,
};
use bss_orders::domain::gate::vector::{Applicability, Outcome, OutcomeKey, Vector, Verdict};
use bss_orders::domain::transition::GuardVerdict;
use bss_orders_sdk::catalog::Reason;
use bss_orders_sdk::commercial::{
    AcceptedVersionRef, AssessmentRef, CodecError, MAX_VERSION_BYTES, MalformedKind, OrderPin,
    SelectionEvidence, encode_new_sale_query,
};
use bss_orders_sdk::gate_report::{CatalogSelection, GateFailure, GateOutcome, GateReport};
use bss_orders_sdk::models::OrderVersion;
use bss_pricing_sdk::acceptance::Term;
use bss_pricing_sdk::acceptance::{AcceptanceReceipt, CommercialReason};
use bss_pricing_sdk::digest::{
    billing_terms_digest, money_digest, request_digest, selected_bindings_digest, template_digest,
    terms_digest,
};
use bss_pricing_sdk::read::{
    AcceptedBinding, BindingSelection, ChargeKind, ImmutablePrice, PriceModel, PriceState,
    ResolvedBindings, ResolvedCell,
};
use bss_pricing_sdk::terms::{
    BillingAnchor, BillingCycle, BillingTerms, BillingTiming, InputSource, InvoiceInputs, Rounding,
    TermsSource, Timezone,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use time::{Date, Month, OffsetDateTime, Time};
use uuid::Uuid;

const ORDER: Uuid = Uuid::from_u128(0x0de5);
const PLAN: Uuid = Uuid::from_u128(1);
const REV: Uuid = Uuid::from_u128(2);
const ITEM: Uuid = Uuid::from_u128(3);
const ITEM_B: Uuid = Uuid::from_u128(5);
const RESOURCE: Uuid = Uuid::from_u128(50);
const PAYER: Uuid = Uuid::from_u128(51);
const SELLER: Uuid = Uuid::from_u128(52);
const POLICY: Uuid = Uuid::from_u128(60);
const CANDIDATE: i32 = 2;

fn line_id(n: u128) -> Uuid {
    Uuid::from_u128(100 + n)
}
fn day() -> Date {
    Date::from_calendar_date(2026, Month::October, 22).unwrap()
}
fn activation() -> Date {
    Date::from_calendar_date(2026, Month::November, 3).unwrap()
}
fn anchor_at() -> OffsetDateTime {
    activation().with_time(Time::MIDNIGHT).assume_utc()
}
fn hex(digest: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    digest.iter().fold(String::new(), |mut text, b| {
        write!(text, "{b:02x}").unwrap();
        text
    })
}
fn axes() -> TenantAxes {
    TenantAxes {
        resource: RESOURCE,
        payer: PAYER,
        seller: SELLER,
    }
}

fn binding(item: Uuid, dim: Option<&str>) -> AcceptedBinding {
    let mut price = ImmutablePrice {
        price_id: Uuid::from_u128(21),
        price_book_entry_id: Uuid::from_u128(20),
        money_digest: [0; 32],
        currency: bss_money::CurrencySpec::try_new("USD".into(), 2).unwrap(),
        model: PriceModel::Flat {
            amount: bss_money::parse_decimal("10").unwrap(),
        },
        minimum_fee: None,
        effective_from: Date::from_calendar_date(2026, Month::January, 1).unwrap(),
        ends_on: None,
        state: PriceState::Approved,
    };
    price.money_digest = money_digest(&price);
    AcceptedBinding {
        item_id: item,
        price_book_entry_id: Uuid::from_u128(20),
        dimension_key: dim.map(|_| "region".to_owned()),
        dimension_value: dim.map(str::to_owned),
        sku_id: Uuid::from_u128(4),
        sku_version: 3,
        sku_code: "VM-SMALL".into(),
        sku_name: "Small VM".into(),
        unit: Some("instance".into()),
        meter: None,
        price,
        kind: ChargeKind::Recurring,
        recurring_period: Some(BillingCycle::Month),
        via_default: false,
        usage_rating_policy: None,
        invoice: InvoiceInputs {
            template: "{sku_name}".into(),
            template_digest: template_digest("{sku_name}"),
            template_source: InputSource::Entry,
            gl_code: "4000".into(),
            tax_category: "standard".into(),
            timing: BillingTiming::Advance,
            rounding: Rounding::HalfEven,
        },
    }
}
fn resolved(items: &[(Uuid, Option<&str>)]) -> ResolvedBindings {
    ResolvedBindings {
        plan_id: PLAN,
        revision_id: REV,
        cells: items
            .iter()
            .map(|(item, dim)| ResolvedCell {
                selection: BindingSelection {
                    item_id: *item,
                    dimension_value: dim.map(str::to_owned),
                },
                binding: Some(binding(*item, *dim)),
            })
            .collect(),
    }
}
fn item(item_id: Uuid, quantity: &str, dim: Option<&str>) -> SelectedItem {
    SelectedItem {
        item_id,
        quantity: quantity.into(),
        selected_dim_value: dim.map(str::to_owned),
    }
}
fn basket_line(n: u128, items: Vec<SelectedItem>) -> BasketLine {
    BasketLine {
        line_id: line_id(n),
        catalog: LineCatalogRequest {
            line_id: line_id(n),
            plan_id: PLAN,
            revision_id: REV,
            items,
        },
        currency: Some("USD".into()),
        contract_ref: None,
    }
}
/// The Pricing projection the snapshot below states, with its real digest.
fn pricing_terms() -> BillingTerms {
    let mut terms = BillingTerms {
        schema_version: 1,
        cycle: BillingCycle::Month,
        anchor: BillingAnchor::SubscriptionStart,
        anchor_at: anchor_at(),
        timezone: Timezone::Utc,
        source: TermsSource::SellerPolicy {
            id: POLICY,
            version: 4,
        },
        digest: [0; 32],
    };
    terms.digest = billing_terms_digest(&terms);
    terms
}
fn terms(line: Uuid) -> ResolvedTerms {
    ResolvedTerms {
        line_id: line,
        billing_terms: BillingTermsRef {
            digest: hex(&pricing_terms().digest),
        },
        term: EvaluationTerm::FixedPeriods { count: 12 },
        earliest_start: None,
        provenance: TermsProvenance::SellerPolicy {
            version: "4".into(),
        },
        snapshot: Some(BillingTermsSnapshot {
            schema_version: 1,
            cycle: EvaluationCycle::Month,
            anchor: TermsAnchor::SubscriptionStart,
            anchor_at: anchor_at(),
            timezone: "UTC".into(),
            source: TermsSnapshotSource::SellerPolicy {
                id: POLICY,
                version: 4,
            },
        }),
    }
}
fn basis(lines: &[BasketLine]) -> DateBasis {
    let row = PolicyRow {
        policy_id: PLATFORM_DEFAULT_POLICY_ID,
        resource_tenant_id: None,
        service_activation_required: false,
        acceptance_due_required: false,
        revision: 1,
    };
    let policy = DatePolicySnapshot::effective(RESOURCE, &[row]).unwrap();
    let supplied = lines.iter().map(|l| {
        (
            l.line_id,
            SuppliedDates {
                contract_effective: None,
                service_activation: Some(activation()),
                acceptance_due: None,
            },
        )
    });
    DateBasis::resolve(RESOURCE, policy, day(), supplied)
}

/// Everything one basket's line requests read, owned so a test can perturb one member.
struct Inputs {
    lines: Vec<BasketLine>,
    bindings: BTreeMap<Uuid, Arc<ResolvedBindings>>,
    terms: BTreeMap<Uuid, ResolvedTerms>,
    keys: BTreeMap<Uuid, Vec<bss_orders::domain::gate::answers::OverlapKey>>,
    market: Option<Market>,
    basis: DateBasis,
    policy: AcceptancePolicy,
}
impl Inputs {
    fn new(lines: Vec<BasketLine>) -> Self {
        let bindings = lines
            .iter()
            .map(|l| {
                let cells: Vec<(Uuid, Option<&str>)> = l
                    .catalog
                    .items
                    .iter()
                    .map(|i| (i.item_id, i.selected_dim_value.as_deref()))
                    .collect();
                (l.line_id, Arc::new(resolved(&cells)))
            })
            .collect();
        let terms = lines
            .iter()
            .map(|l| (l.line_id, terms(l.line_id)))
            .collect();
        let basis = basis(&lines);
        let keys = lines
            .iter()
            .map(|line| {
                let keys = line
                    .catalog
                    .items
                    .iter()
                    .map(|selection| {
                        let product = format!("product-{}", line.line_id);
                        let scope = format!("scope-{}", line.line_id);
                        bss_orders::domain::gate::answers::OverlapKey {
                            line_id: line.line_id,
                            catalog_subscription_product_key: product.clone(),
                            overlap_scope_key: scope.clone(),
                            item_key: Some(bss_orders::domain::gate::item_keys::ItemOverlapKey {
                                profile: "item_keys_v1".into(),
                                selection: bss_orders::domain::gate::answers::OverlapKeyLine {
                                    line_id: line.line_id,
                                    plan_revision_id: REV,
                                    item_id: selection.item_id,
                                    dimension_value: selection.selected_dim_value.clone(),
                                    paid_recurring_sku: Uuid::from_u128(4),
                                    sku_version: 3,
                                },
                                catalog_subscription_product_key: product,
                                overlap_scope_key: scope,
                                classification:
                                    bss_orders::domain::gate::item_keys::ClassificationProvenance {
                                        source: "test-provider".into(),
                                        reference: "classification".into(),
                                        version: "1".into(),
                                    },
                            }),
                        }
                    })
                    .collect();
                (line.line_id, keys)
            })
            .collect();
        Self {
            lines,
            bindings,
            terms,
            keys,
            market: Some(Market {
                currency: "USD".into(),
                region: "US".into(),
            }),
            basis,
            policy: AcceptancePolicy {
                version: "7".into(),
                hold_seconds: 600,
            },
        }
    }
    fn facts(&self) -> CommercialFacts<'_> {
        CommercialFacts {
            order_id: ORDER,
            axes: axes(),
            lines: &self.lines,
            resolved_bindings: &self.bindings,
            terms: &self.terms,
            keys: &self.keys,
            market: self.market.as_ref(),
            basis: &self.basis,
            policy: &self.policy,
        }
    }
}
fn two_lines() -> Inputs {
    Inputs::new(vec![
        basket_line(1, vec![item(ITEM, "2", None)]),
        basket_line(2, vec![item(ITEM, "1.5", Some("eu"))]),
    ])
}

#[test]
fn every_query_member_is_sourced_from_the_basket_the_answers_and_the_frozen_basis() {
    let inputs = two_lines();
    let requests = build_line_requests(&inputs.facts(), CANDIDATE).unwrap();
    assert_eq!(requests.len(), 2);
    let r = &requests[1];
    let q = &r.query;
    assert_eq!(r.line_id, line_id(2));
    assert_eq!(q.tenant_axes.seller_tenant_id, SELLER);
    assert_eq!(q.tenant_axes.payer_tenant_id, PAYER);
    assert_eq!(q.tenant_axes.resource_tenant_id, RESOURCE);
    assert_eq!(q.order_id, ORDER);
    assert_eq!(q.order_version, 2, "the reserved candidate");
    assert_eq!(q.line_id, line_id(2));
    assert_eq!((q.plan_id, q.plan_revision_id), (PLAN, REV));
    assert_eq!(
        q.selections,
        vec![BindingSelection {
            item_id: ITEM,
            dimension_value: Some("eu".into()),
        }]
    );
    assert_eq!(q.quantity, bss_money::parse_decimal("1.5").unwrap());
    assert_eq!(q.market.currency, "USD");
    assert_eq!(q.market.region.as_deref(), Some("US"));
    assert_eq!(
        q.start_at,
        activation().with_time(Time::MIDNIGHT).assume_utc(),
        "service activation at 00:00 UTC"
    );
    assert_eq!(q.term, Term::FixedPeriods { count: 12 });
    assert_eq!(q.billing_terms, pricing_terms());
    assert_eq!(q.hold_policy_version, 7);
}

#[test]
fn digests_equal_the_pricing_digest_functions_over_the_same_inputs() {
    let inputs = two_lines();
    for r in build_line_requests(&inputs.facts(), CANDIDATE).unwrap() {
        let q = &r.query;
        assert_eq!(r.request_digest, request_digest(q));
        assert_eq!(
            q.resolved_bindings_digest,
            selected_bindings_digest(&inputs.bindings[&r.line_id], &q.selections).unwrap()
        );
        assert_eq!(
            q.billing_terms.digest,
            billing_terms_digest(&q.billing_terms)
        );
        assert_eq!(
            hex(&q.billing_terms.digest),
            inputs.terms[&r.line_id].billing_terms.digest
        );
    }
}

#[test]
fn keys_are_stable_per_attempt_and_distinct_per_line_and_candidate() {
    let inputs = two_lines();
    let a = build_line_requests(&inputs.facts(), CANDIDATE).unwrap();
    let b = build_line_requests(&inputs.facts(), CANDIDATE).unwrap();
    let keys: Vec<_> = a.iter().map(|r| r.key.clone()).collect();
    assert_eq!(keys, b.iter().map(|r| r.key.clone()).collect::<Vec<_>>());
    assert_eq!(keys[0], format!("{ORDER}/2/{}/submit", line_id(1)));
    assert_ne!(keys[0], keys[1]);
    let next = build_line_requests(&inputs.facts(), CANDIDATE + 1).unwrap();
    assert_ne!(
        next[0].key, keys[0],
        "another attempt's candidate, another key"
    );
    assert_ne!(next[0].request_digest, a[0].request_digest);
}

#[test]
fn the_stored_line_requests_are_canonical_and_byte_stable() {
    let inputs = two_lines();
    let encode = || {
        let requests = build_line_requests(&inputs.facts(), CANDIDATE).unwrap();
        serde_json::to_vec(&line_requests_value(&requests, &frozen_order(&inputs.lines)).unwrap())
            .unwrap()
    };
    let first = encode();
    assert_eq!(first, encode(), "same inputs, identical bytes");
    let value: serde_json::Value = serde_json::from_slice(&first).unwrap();
    let object = value.as_object().unwrap();
    assert_eq!(object.len(), 3, "two lines and the order member");
    assert!(object.contains_key(ORDER_MEMBER));
    let requests = build_line_requests(&inputs.facts(), CANDIDATE).unwrap();
    for r in &requests {
        let entry = &object[&r.line_id.to_string()];
        let members: Vec<&str> = entry
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(members.len(), 4, "{members:?}");
        assert_eq!(entry["key"], serde_json::json!(r.key));
        assert_eq!(
            entry["item_keys_v1"][0]["overlap_scope_key"],
            serde_json::json!(format!("scope-{}", r.line_id))
        );
        assert_eq!(
            entry["request_digest"],
            serde_json::json!(hex(&r.request_digest))
        );
        assert_eq!(entry["query"], encode_new_sale_query(&r.query).unwrap());
        // Exact decimal text and the candidate as an unsigned scalar, never a JSON number.
        assert_eq!(
            entry["query"]["quantity"],
            serde_json::json!(r.query.quantity.to_string())
        );
        assert_eq!(entry["query"]["order_version"], serde_json::json!("2"));
    }
}

#[test]
fn a_bundle_line_carries_its_one_quantity_and_mixed_quantities_are_unsupported() {
    let same = Inputs::new(vec![basket_line(
        1,
        vec![item(ITEM, "2", None), item(ITEM_B, "2.0", None)],
    )]);
    let requests = build_line_requests(&same.facts(), CANDIDATE).unwrap();
    assert_eq!(
        requests[0].query.quantity,
        bss_money::parse_decimal("2").unwrap()
    );
    assert_eq!(requests[0].query.selections.len(), 2);

    let mixed = Inputs::new(vec![basket_line(
        1,
        vec![item(ITEM, "2", None), item(ITEM_B, "3", None)],
    )]);
    assert!(matches!(
        build_line_requests(&mixed.facts(), CANDIDATE),
        Err(CommercialError::Unsupported { line_id: l, .. }) if l == line_id(1)
    ));
}

#[test]
fn nothing_is_defaulted_a_missing_or_disagreeing_input_refuses_the_line() {
    let line = line_id(1);
    let check = |mutate: &dyn Fn(&mut Inputs), expected: fn(&CommercialError) -> bool| {
        let mut inputs = two_lines();
        mutate(&mut inputs);
        let error = build_line_requests(&inputs.facts(), CANDIDATE).unwrap_err();
        assert!(expected(&error), "{error:?}");
    };
    let incomplete = |e: &CommercialError| matches!(e, CommercialError::Incomplete { .. });
    let unsupported = |e: &CommercialError| matches!(e, CommercialError::Unsupported { .. });
    let mismatch = |e: &CommercialError| matches!(e, CommercialError::Mismatch { .. });
    check(
        &|i| {
            i.terms.get_mut(&line).unwrap().snapshot = None;
        },
        incomplete,
    );
    check(
        &|i| {
            i.terms.remove(&line);
        },
        incomplete,
    );
    check(&|i| i.market = None, incomplete);
    check(
        &|i| {
            i.keys.remove(&line);
        },
        incomplete,
    );
    check(
        &|i| {
            i.bindings.remove(&line);
        },
        incomplete,
    );
    check(
        &|i| {
            i.terms.get_mut(&line).unwrap().billing_terms.digest = "00".repeat(32);
        },
        mismatch,
    );
    check(
        &|i| {
            i.terms
                .get_mut(&line)
                .unwrap()
                .snapshot
                .as_mut()
                .unwrap()
                .timezone = "Europe/Berlin".into();
        },
        unsupported,
    );
    check(&|i| i.policy.version = "dev-stub-1".into(), unsupported);
    check(&|i| i.policy.version = "07".into(), unsupported);
    check(
        &|i| {
            Arc::make_mut(i.bindings.get_mut(&line).unwrap()).cells[0]
                .selection
                .dimension_value = Some("us".into());
        },
        mismatch,
    );
    let inputs = two_lines();
    assert!(matches!(
        build_line_requests(&inputs.facts(), 0),
        Err(CommercialError::Unsupported { .. })
    ));
}

#[test]
fn the_authorization_fact_fingerprint_is_stable_and_binds_every_axis() {
    use bss_orders::domain::idempotency::{FingerprintAxes, authorization_fact_fingerprint};
    let axes = FingerprintAxes {
        seller_tenant_id: SELLER,
        resource_tenant_id: RESOURCE,
        payer_tenant_id: PAYER,
    };
    let fp = authorization_fact_fingerprint(ORDER, &axes).unwrap();
    assert!(fp.starts_with("af1:") && fp.len() == 4 + 64, "{fp}");
    assert_eq!(fp, authorization_fact_fingerprint(ORDER, &axes).unwrap());
    let moved = [
        FingerprintAxes {
            payer_tenant_id: Uuid::from_u128(99),
            ..axes
        },
        FingerprintAxes {
            resource_tenant_id: Uuid::from_u128(99),
            ..axes
        },
        FingerprintAxes {
            seller_tenant_id: Uuid::from_u128(99),
            ..axes
        },
    ];
    for other in moved {
        assert_ne!(fp, authorization_fact_fingerprint(ORDER, &other).unwrap());
    }
    assert_ne!(
        fp,
        authorization_fact_fingerprint(Uuid::from_u128(1), &axes).unwrap()
    );
}

/// The example development fixture's billing terms state their snapshot with its real digest,
/// and its acceptance policy a canonical version, so a stubbed submit can freeze its requests.
#[test]
fn the_example_fixture_states_a_freezable_terms_snapshot_and_policy() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../config/gate-dev-stubs.example.json"
    );
    let fixture: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let expected = {
        let mut t = BillingTerms {
            schema_version: 1,
            cycle: BillingCycle::Month,
            anchor: BillingAnchor::SubscriptionStart,
            anchor_at: Date::from_calendar_date(2026, Month::January, 1)
                .unwrap()
                .with_time(Time::MIDNIGHT)
                .assume_utc(),
            timezone: Timezone::Utc,
            source: TermsSource::ExplicitOrder,
            digest: [0; 32],
        };
        t.digest = billing_terms_digest(&t);
        t
    };
    let terms = fixture["terms"].as_object().unwrap();
    assert!(!terms.is_empty());
    for stated in terms.values() {
        let stated: ResolvedTerms = serde_json::from_value(stated.clone()).unwrap();
        assert!(stated.snapshot.is_some());
        assert_eq!(stated.billing_terms.digest, hex(&expected.digest));
    }
    let policy: AcceptancePolicy =
        serde_json::from_value(fixture["acceptance_policy"].clone()).unwrap();
    assert_eq!(policy.version, "1");
}

// ---- M2-6: the frozen order, the recorded results, the pin, the totals and the limits ----

fn money(n: &str, d: &str) -> ExactMoney {
    ExactMoney::new(n, d, "USD", 2).unwrap()
}
fn amount(n: &str, display: &str) -> Amount {
    Amount {
        exact: money(n, "1"),
        display: display.into(),
    }
}
fn totals(n: &str, display: &str) -> Totals {
    Totals {
        gross: amount(n, display),
        net: amount(n, display),
        discount: Availability::Unavailable,
    }
}
fn declaration(kind: EvaluationChargeKind) -> TotalsDeclaration {
    TotalsDeclaration {
        charge_kind: kind,
        amount_basis: "rolling_annualized".into(),
        period_evidence: serde_json::json!({"cycle": "month"}),
        item_breakdown: serde_json::json!([]),
        recurring_by_cycle: serde_json::json!({"month": "10"}),
        exclusions: serde_json::json!({}),
    }
}
/// A storable evaluation of `lines`: exact integers, canonical displays, every declaration.
fn evaluation(lines: &[BasketLine]) -> OrderEvaluation {
    OrderEvaluation {
        assessment_id: Uuid::from_u128(70),
        estimate: true,
        currency: "USD".into(),
        currency_scale: 2,
        lines: lines
            .iter()
            .map(|l| LineEvaluation {
                line_id: l.line_id,
                totals: totals("10", "10"),
                usage_flagged: false,
                committed_components: None,
                declaration: Some(declaration(EvaluationChargeKind::Recurring)),
                usage_declaration: None,
            })
            .collect(),
        order_totals: totals("20", "20"),
        order_committed_components: None,
        order_tcv_carrier: None,
        order_declaration: Some(declaration(EvaluationChargeKind::Recurring)),
        order_usage_declaration: None,
        tcv: Some(Tcv {
            exact: money("240", "1"),
            display: "240".into(),
            posting_estimate: "240.00".into(),
            basis: Some(TcvBasis::RollingAnnualized),
            evidence: Some(serde_json::json!({"policy_version": "1"})),
        }),
        promotion_status: Availability::Unavailable,
        request_digest: "digest".into(),
    }
}
/// The run's frozen outcome vector: one passed order-level outcome and one unevaluable line one.
fn frozen_vector(lines: &[BasketLine]) -> Vector {
    let outcome = |predicate, line_id, verdict, reason| Outcome {
        key: OutcomeKey {
            predicate,
            line_id,
            item_id: None,
            selection: CatalogSelection::NotSelected,
        },
        verdict,
        reason,
        applicability: Applicability::Required,
        producer_results: Vec::new(),
        upstream_detail: None,
    };
    Vector::build(vec![
        outcome(
            PredicateId::CatalogRevisionCurrent,
            lines.first().map(|l| l.line_id),
            Verdict::Passed,
            None,
        ),
        outcome(
            PredicateId::CatalogRevisionAvailable,
            lines.first().map(|l| l.line_id),
            Verdict::Passed,
            None,
        ),
    ])
    .unwrap()
}
/// The run's gate report as the submit freezes it: one passed order outcome and one failed,
/// line- and item-scoped outcome with its failure entry.
fn frozen_report() -> GateReport {
    GateReport::new(
        Uuid::from_u128(70),
        "orders-gate-mapping/1",
        vec![
            GateOutcome::new("order.has-lines", "passed", "required"),
            GateOutcome::new("catalog.revision-available", "failed", "required")
                .with_line(line_id(2))
                .with_item(ITEM)
                .with_selection(CatalogSelection::Selected("eu".into()))
                .with_reason(Some(Reason::NoLines)),
        ],
        vec![GateFailure::new(
            Some(line_id(2)),
            Reason::NoLines,
            "catalog.revision-available",
        )],
    )
}

fn frozen_order(lines: &[BasketLine]) -> FrozenOrder {
    FrozenOrder {
        assessment_id: Uuid::from_u128(70),
        assessed_at: anchor_at(),
        resolve_date: day(),
        evaluation_request: None,
        evaluation: evaluation(lines),
        report: frozen_report(),
        correlation_id: Some(Uuid::from_u128(71)),
        vector: frozen_vector(lines),
    }
}

#[test]
fn frozen_lines_preserve_product_identity_independently_of_scope() {
    let inputs = two_lines();
    let requests = build_line_requests(&inputs.facts(), CANDIDATE).unwrap();
    let value = line_requests_value(&requests, &frozen_order(&inputs.lines)).unwrap();
    assert_eq!(
        value[line_id(1).to_string()]["item_keys_v1"][0]["catalog_subscription_product_key"],
        serde_json::json!(format!("product-{}", line_id(1)))
    );
}

#[test]
fn legacy_frozen_lines_never_infer_product_identity_from_scope() {
    let inputs = two_lines();
    let requests = build_line_requests(&inputs.facts(), CANDIDATE).unwrap();
    let mut value = line_requests_value(&requests, &frozen_order(&inputs.lines)).unwrap();
    for id in [line_id(1), line_id(2)] {
        let entry = value[id.to_string()].as_object_mut().unwrap();
        entry.remove("item_keys_v1");
        entry.insert(
            "overlap_scope_key".into(),
            serde_json::json!(format!("scope-{id}")),
        );
    }
    let decoded = decode_line_requests(&value).unwrap();
    assert!(
        decoded
            .lines
            .iter()
            .all(|line| line.catalog_subscription_product_key.is_none()
                && line.overlap_scope_key.is_some())
    );
    value[line_id(1).to_string()]["catalog_subscription_product_key"] = serde_json::json!(12);
    assert!(decode_line_requests(&value).is_err());
}

#[test]
fn the_stored_attempt_decodes_back_to_its_requests_and_order() {
    let inputs = two_lines();
    let requests = build_line_requests(&inputs.facts(), CANDIDATE).unwrap();
    let order = frozen_order(&inputs.lines);
    let value = line_requests_value(&requests, &order).unwrap();
    let decoded = decode_line_requests(&value).unwrap();
    assert_eq!(decoded.order, order);
    assert_eq!(
        decoded.order.evaluation_request, None,
        "legacy attempts have no invented Rating input"
    );
    assert_eq!(decoded.lines.len(), 2);
    for r in &requests {
        let line = decoded
            .lines
            .iter()
            .find(|l| l.line_id == r.line_id)
            .unwrap();
        assert_eq!(line.query, r.query);
        assert_eq!(line.key, r.key);
        assert_eq!(line.request_digest, r.request_digest);
        assert_eq!(line.overlap_scope_key, r.overlap_scope_key);
        assert_eq!(line.overlap_item_keys, r.overlap_item_keys);
        assert_eq!(
            line.catalog_subscription_product_key,
            r.catalog_subscription_product_key
        );
    }
    // Evidence that cannot be read is never committed from.
    let tamper = |f: &dyn Fn(&mut serde_json::Value)| {
        let mut v = value.clone();
        f(&mut v);
        decode_line_requests(&v).is_err()
    };
    assert!(tamper(&|v| {
        v.as_object_mut().unwrap().remove(ORDER_MEMBER);
    }));
    let first = line_id(1).to_string();
    assert!(tamper(
        &|v| v[&first]["request_digest"] = serde_json::json!("00".repeat(32))
    ));
    assert!(tamper(&|v| v[&first]["extra"] = serde_json::json!(1)));
    assert!(tamper(
        &|v| v[&first]["query"]["quantity"] = serde_json::json!("3")
    ));
    // D-231: the gate report and the outcome vector are frozen with the order; a vector that
    // breaks the identity rules (a reason on a passed outcome) never decodes.
    assert_eq!(decoded.order.vector, frozen_vector(&inputs.lines));
    assert_eq!(decoded.order.report, order.report);
    assert!(tamper(&|v| {
        v[ORDER_MEMBER].as_object_mut().unwrap().remove("vector");
    }));
    assert!(tamper(
        &|v| v[ORDER_MEMBER]["vector"][0]["reason"] = serde_json::json!("no-lines")
    ));
}

/// Dropping any request member loses the exact Rating input reserved before dispatch.
#[test]
fn the_stored_attempt_preserves_the_actual_rating_request() {
    let inputs = two_lines();
    let requests = build_line_requests(&inputs.facts(), CANDIDATE).unwrap();
    let mut value = line_requests_value(&requests, &frozen_order(&inputs.lines)).unwrap();
    let request = serde_json::json!({
        "tenant_axes": {"seller": SELLER, "payer": PAYER, "resource": RESOURCE},
        "assessed_at": "2026-10-21T12:34:56Z",
        "acceptance_policy": {"version":"47", "hold_seconds":3600},
        "assessment_id": Uuid::from_u128(701),
        "resolve_date": "2026-10-21",
        "market": {"currency": "EUR", "region": "rating-region"},
        "contract_id": Uuid::from_u128(702),
        "lines": [{
            "line_id": line_id(2), "plan_revision_id": Uuid::from_u128(703),
            "plan_id": PLAN, "start_at": "2026-10-25T00:00:00Z",
            "resolved_terms": terms(line_id(2)),
            "term": "rolling", "cycle": "month",
            "items": [{"item_id": ITEM_B, "quantity": "7.25", "chains": [{
                "dim_value": "requested-region", "binding": {
                    "native_binding": bss_orders_sdk::commercial::encode_accepted_binding(&inputs.bindings[&line_id(2)].cells[0].binding.clone().unwrap()).unwrap(),
                    "item_id": ITEM_B, "price_book_entry_id": Uuid::from_u128(704),
                    "price_id": Uuid::from_u128(705), "money_digest": "ab".repeat(32),
                    "currency": "EUR", "currency_scale": 2, "sku_id": Uuid::from_u128(706),
                    "sku_version": 17, "dimension_key": "region", "dimension_value": "default-region",
                    "kind": "recurring", "recurring_period": "month", "via_default": true
                }
            }]}]
        }]
    });
    let _: bss_orders::domain::gate::answers::OrderEvaluationRequest =
        serde_json::from_value(request.clone()).unwrap();
    value[ORDER_MEMBER]["evaluation_request"] = request.clone();
    let decoded = decode_line_requests(&value).unwrap();
    let encoded = line_requests_value(&requests, &decoded.order).unwrap();
    assert_eq!(encoded[ORDER_MEMBER]["evaluation_request"], request);
    // Old frozen snapshots omit every enrichment; decoding must retain that absence.
    let mut legacy = request;
    for key in ["tenant_axes", "assessed_at", "acceptance_policy"] {
        legacy.as_object_mut().unwrap().remove(key);
    }
    let line = legacy["lines"][0].as_object_mut().unwrap();
    for key in ["plan_id", "start_at", "resolved_terms"] {
        line.remove(key);
    }
    legacy["lines"][0]["items"][0]["chains"][0]["binding"]
        .as_object_mut()
        .unwrap()
        .remove("native_binding");
    value[ORDER_MEMBER]["evaluation_request"] = legacy.clone();
    let decoded = decode_line_requests(&value).unwrap();
    let encoded = line_requests_value(&requests, &decoded.order).unwrap();
    assert_eq!(encoded[ORDER_MEMBER]["evaluation_request"], legacy);
}

/// The receipt the development acceptance double issues for `r` (its formula: Pricing's digest
/// functions, `hold_until = accepted_at + hold_seconds`).
fn stub_receipt(inputs: &Inputs, line: Uuid, candidate: i32) -> AcceptanceReceipt {
    let requests = build_line_requests(&inputs.facts(), candidate).unwrap();
    let r = requests.into_iter().find(|r| r.line_id == line).unwrap();
    bss_orders::domain::gate::commercial::deterministic_receipt(
        r.query,
        &inputs.bindings[&line],
        Uuid::from_u128(0xacce),
        anchor_at(),
        inputs.policy.hold_seconds,
    )
    .unwrap()
}

#[test]
fn the_stub_receipt_pins_round_trip_and_verify_against_the_frozen_request() {
    let inputs = two_lines();
    let requests = build_line_requests(&inputs.facts(), CANDIDATE).unwrap();
    let value = line_requests_value(&requests, &frozen_order(&inputs.lines)).unwrap();
    let decoded = decode_line_requests(&value).unwrap();
    for line in &decoded.lines {
        let receipt = stub_receipt(&inputs, line.line_id, CANDIDATE);
        assert_eq!(receipt.request_digest, line.request_digest);
        assert_eq!(
            receipt.hold_until - receipt.accepted_at,
            time::Duration::seconds(600)
        );
        // The recorded result round-trips.
        let recorded = receipt_result_value(
            line.line_id,
            &ReceiptResult::Accepted(Box::new(receipt.clone())),
        )
        .unwrap();
        assert_eq!(
            decode_receipt_result(&recorded).unwrap(),
            ReceiptResult::Accepted(Box::new(receipt.clone()))
        );
        let pin = OrderPin::new(
            AssessmentRef {
                assessment_id: decoded.order.assessment_id,
                assessed_at: decoded.order.assessed_at,
                resolve_date: decoded.order.resolve_date,
            },
            AcceptedVersionRef {
                order_id: ORDER,
                order_version: OrderVersion::try_from(i64::from(CANDIDATE)).unwrap(),
                line_id: line.line_id,
            },
            receipt.clone(),
        )
        .unwrap();
        let decoded_pin = OrderPin::decode(&pin.encode().unwrap()).unwrap();
        assert_eq!(decoded_pin, pin);
        let evidence = SelectionEvidence {
            acceptance_id: receipt.acceptance_id,
            request_digest: line.request_digest,
            terms_digest: terms_digest(&line.query, &receipt.bindings),
            evaluated_bindings: &receipt.bindings,
        };
        decoded_pin.verify_selection(&line.query, evidence).unwrap();
        // A receipt for another candidate's request never verifies (D-192 fail closed).
        let other = stub_receipt(&inputs, line.line_id, CANDIDATE + 1);
        assert_eq!(
            decoded_pin.verify_selection(&other.query, evidence),
            Err(CodecError::Mismatch)
        );
    }
}

/// The Rating request the gate builds for `inputs` (the orchestrator's `evaluation_request`).
fn evaluated_request(inputs: &Inputs) -> OrderEvaluationRequest {
    use bss_orders::domain::gate::answers::TermIntentInput;
    use bss_orders::domain::gate::orchestrate::{
        Basket, GateTarget, LineTermIntent, LineTerms, evaluation_request,
    };
    let basket = Basket {
        assessed_at: anchor_at(),
        target: GateTarget::Submit { order_id: ORDER },
        assessment_id: Uuid::from_u128(70),
        axes: axes(),
        contract_id: None,
        lines: inputs.lines.clone(),
        terms: inputs
            .lines
            .iter()
            .map(|l| {
                (
                    l.line_id,
                    LineTerms {
                        intent: LineTermIntent::Stated(TermIntentInput::Fixed { months: 12 }),
                        cycle: Some(EvaluationCycle::Month),
                    },
                )
            })
            .collect(),
        resolve_date: day(),
        dates: inputs.basis.clone(),
    };
    let bindings = inputs
        .bindings
        .iter()
        .map(|(line, resolved)| {
            (
                *line,
                resolved
                    .cells
                    .iter()
                    .map(|c| c.binding.clone().unwrap())
                    .collect(),
            )
        })
        .collect();
    let terms: Vec<ResolvedTerms> = inputs.terms.values().cloned().collect();
    evaluation_request(
        &basket,
        &bindings,
        Some(&terms),
        inputs.market.as_ref(),
        Some(&inputs.policy),
    )
    .unwrap()
    .unwrap()
}

/// D-197 / R3: each frozen query equals the Rating request its totals were evaluated from;
/// members a legacy snapshot omits are not compared and never fabricated.
#[test]
fn frozen_requests_must_equal_the_evaluated_request() {
    let inputs = two_lines();
    let evaluated = evaluated_request(&inputs);
    let requests = build_line_requests(&inputs.facts(), CANDIDATE).unwrap();
    for r in &requests {
        assert_eq!(
            verify_evaluated_query(&r.query, &evaluated),
            Ok(()),
            "{}",
            r.line_id
        );
    }
    // R3: Rating priced 12 periods; the frozen query says 24.
    let mut longer = two_lines();
    longer.terms.get_mut(&line_id(1)).unwrap().term = EvaluationTerm::FixedPeriods { count: 24 };
    let longer_requests = build_line_requests(&longer.facts(), CANDIDATE).unwrap();
    assert_eq!(
        verify_evaluated_query(&longer_requests[0].query, &evaluated),
        Err("evaluated term")
    );
    let differs = |f: &dyn Fn(&mut OrderEvaluationRequest), what: &'static str| {
        let mut request = evaluated.clone();
        f(&mut request);
        assert_eq!(
            verify_evaluated_query(&requests[0].query, &request),
            Err(what)
        );
    };
    differs(&|r| r.market.currency = "EUR".into(), "evaluated market");
    differs(&|r| r.market.region = "EU".into(), "evaluated market");
    differs(
        &|r| r.lines.retain(|l| l.line_id != line_id(1)),
        "evaluated line",
    );
    differs(
        &|r| r.lines[0].plan_revision_id = Uuid::nil(),
        "evaluated revision",
    );
    differs(
        &|r| r.lines[0].cycle = EvaluationCycle::Year,
        "evaluated cycle",
    );
    differs(
        &|r| r.lines[0].items[0].quantity = "9".into(),
        "evaluated selections",
    );
    differs(
        &|r| r.acceptance_policy.as_mut().unwrap().version = "8".into(),
        "evaluated hold policy version",
    );
    let mut legacy = evaluated.clone();
    legacy.tenant_axes = None;
    legacy.acceptance_policy = None;
    for line in &mut legacy.lines {
        line.plan_id = None;
        line.start_at = None;
        line.resolved_terms = None;
    }
    for r in &requests {
        assert_eq!(
            verify_evaluated_query(&r.query, &legacy),
            Ok(()),
            "legacy {}",
            r.line_id
        );
    }
    // The pre-reservation build refuses the mismatch; a legacy order without a request builds.
    let mut order = frozen_order(&inputs.lines);
    order.evaluation_request = Some(evaluated);
    assert!(frozen_line_requests(&inputs.facts(), CANDIDATE, &order).is_ok());
    assert!(matches!(
        frozen_line_requests(&longer.facts(), CANDIDATE, &order),
        Err(CommercialError::Mismatch { line_id: l, what: "evaluated term" }) if l == line_id(1)
    ));
    order.evaluation_request = None;
    assert!(frozen_line_requests(&longer.facts(), CANDIDATE, &order).is_ok());
}

/// D-197: the issued receipt's bindings are the evaluated ones; a legacy binding without its
/// native member compares the projected identity.
#[test]
fn the_issued_bindings_must_be_the_evaluated_bindings() {
    let inputs = two_lines();
    let evaluated = evaluated_request(&inputs);
    let requests = build_line_requests(&inputs.facts(), CANDIDATE).unwrap();
    let query = &requests[0].query;
    let receipt = stub_receipt(&inputs, line_id(1), CANDIDATE);
    assert_eq!(
        verify_evaluated_bindings(query, &receipt.bindings, &evaluated),
        Ok(())
    );
    let mut legacy = evaluated.clone();
    legacy.lines[0].items[0].chains[0].binding.native_binding = None;
    assert_eq!(
        verify_evaluated_bindings(query, &receipt.bindings, &legacy),
        Ok(())
    );
    let mut other = receipt.bindings;
    other[0].sku_version += 1;
    assert_eq!(
        verify_evaluated_bindings(query, &other, &evaluated),
        Err("evaluated bindings")
    );
    assert_eq!(
        verify_evaluated_bindings(query, &[], &evaluated),
        Err("evaluated bindings")
    );
}

#[test]
#[expect(
    clippy::cognitive_complexity,
    reason = "one scenario walks every refusal class in order; splitting it would repeat the shared setup"
)]
fn recorded_refusals_settle_business_failures_and_preserve_contract_defects() {
    let refused =
        |r: CommercialReason| recorded_refusal(&toolkit_canonical_errors::CanonicalError::from(r));
    assert_eq!(
        refused(CommercialReason::NotSellable),
        Some(CommercialReason::NotSellable)
    );
    assert_eq!(
        refused(CommercialReason::HoldExpired),
        Some(CommercialReason::HoldExpired)
    );
    // Terminal unsupported terms settle even though Pricing uses InvalidArgument.
    for reason in [
        CommercialReason::UnsupportedTerms,
        CommercialReason::UnsupportedModel,
        CommercialReason::UnsupportedWindow,
        CommercialReason::UnsupportedScope,
        CommercialReason::MissingBillingTerms,
        CommercialReason::BillingCycleMismatch,
        CommercialReason::UnalignedBillingAnchor,
        CommercialReason::InvalidTermCount,
    ] {
        assert_eq!(refused(reason), Some(reason), "{reason:?}");
        assert_eq!(
            refusal_disposition(reason),
            RefusalDisposition::Composite(Reason::BillingTermsUnsupported)
        );
        let value = receipt_result_value(line_id(1), &ReceiptResult::Refused(reason)).unwrap();
        assert_eq!(
            decode_receipt_result(&value).unwrap(),
            ReceiptResult::Refused(reason)
        );
    }
    // Request consistency defects remain unrecorded; phase B answers Integration.
    for reason in [
        CommercialReason::BillingTermsDigestMismatch,
        CommercialReason::MoneyDigestMismatch,
        CommercialReason::TemplateDigestMismatch,
        CommercialReason::InvalidMoney,
        CommercialReason::CurrencyMismatch,
        CommercialReason::MarketMismatch,
        CommercialReason::IncompleteSelection,
        CommercialReason::BindingEntryMismatch,
        CommercialReason::DimensionMismatch,
        CommercialReason::MissingRatingPolicy,
        CommercialReason::MeterPolicyMismatch,
        CommercialReason::InvalidQuantity,
        CommercialReason::IncompleteCommercialInputs,
        CommercialReason::InvalidTiers,
    ] {
        assert_eq!(refused(reason), None, "{reason:?}");
        assert!(
            decode_receipt_result(&serde_json::json!({"refused": {"reason": reason.code()}}))
                .is_err()
        );
    }
    assert_eq!(refused(CommercialReason::PermissionDenied), None);
    for reason in [
        CommercialReason::NotSellable,
        CommercialReason::ResolutionChanged,
        CommercialReason::MarketChanged,
        CommercialReason::SkuRetired,
        CommercialReason::PriceClosed,
    ] {
        assert_eq!(
            refusal_disposition(reason),
            RefusalDisposition::Composite(Reason::CatalogPredicateFailed)
        );
    }
    assert_eq!(
        refusal_disposition(CommercialReason::HoldExpired),
        RefusalDisposition::BindingAdmission(Reason::OrderBindingExpired)
    );
    // An activation outside the accepted window is the binding no longer admitting the order.
    assert_eq!(
        refusal_disposition(CommercialReason::ActivationOutsideAcceptedWindow),
        RefusalDisposition::BindingAdmission(Reason::OrderBindingExpired)
    );
    // A key or acceptance conflict is a broken command contract: never recorded, never a
    // settled refusal (phase B answers an integration failure).
    assert_eq!(RECORDED_REFUSALS.len(), 15);
    for reason in CONTRACT_DEFECT_REFUSALS {
        let error = toolkit_canonical_errors::CanonicalError::from(reason);
        assert_eq!(recorded_refusal(&error), None, "{reason:?}");
        assert_eq!(contract_defect(&error), Some(reason));
        assert!(!RECORDED_REFUSALS.contains(&reason));
        assert!(
            decode_receipt_result(&serde_json::json!({"refused": {"reason": reason.code()}}))
                .is_err()
        );
    }
    assert_eq!(
        contract_defect(&toolkit_canonical_errors::CanonicalError::from(
            CommercialReason::NotSellable
        )),
        None
    );
    let value = receipt_result_value(
        line_id(1),
        &ReceiptResult::Refused(CommercialReason::SkuRetired),
    )
    .unwrap();
    assert_eq!(
        value,
        serde_json::json!({"refused": {"reason": "SKU_RETIRED"}})
    );
    assert_eq!(
        decode_receipt_result(&value).unwrap(),
        ReceiptResult::Refused(CommercialReason::SkuRetired)
    );
}

#[test]
fn term_refusals_require_the_commercial_terms_field_and_typed_reason() {
    use toolkit_canonical_errors::CanonicalError;
    use toolkit_canonical_errors::context::InvalidArgument;
    let mut error = CanonicalError::from(CommercialReason::UnsupportedTerms);
    if let CanonicalError::InvalidArgument {
        ctx: InvalidArgument::FieldViolations { field_violations },
        ..
    } = &mut error
    {
        field_violations[0].field = "other".into();
    }
    assert_eq!(recorded_refusal(&error), None);
    if let CanonicalError::InvalidArgument {
        ctx: InvalidArgument::FieldViolations { field_violations },
        ..
    } = &mut error
    {
        field_violations[0].field = "commercial_terms".into();
        field_violations[0].reason = "UNKNOWN".into();
    }
    // The display description still contains UNSUPPORTED_TERMS, but is not authoritative.
    assert_eq!(recorded_refusal(&error), None);
}

#[test]
fn totals_rows_copy_the_evaluation_and_carry_the_tcv_only_on_the_order_recurring_row() {
    let inputs = two_lines();
    let ids: Vec<Uuid> = inputs.lines.iter().map(|l| l.line_id).collect();
    let rows = total_rows(&evaluation(&inputs.lines), &ids, "USD").unwrap();
    assert_eq!(rows.len(), 3);
    for row in &rows[..2] {
        assert_eq!(row.scope, TotalScope::Line);
        assert_eq!(row.charge_kind, EvaluationChargeKind::Recurring);
        assert_eq!(row.gross, Some(("10".to_owned(), "10".to_owned())));
        assert_eq!(row.amount_status, AmountStatus::Committed);
        assert!(row.tcv.is_none());
        assert!(row.discount.is_none());
    }
    let order = &rows[2];
    assert_eq!(
        (order.scope, order.line_id),
        (TotalScope::Order, Uuid::nil())
    );
    let (exact, display, basis, _) = order.tcv.clone().unwrap();
    assert_eq!(
        (exact.as_str(), display.as_str(), basis),
        ("240", "240", TcvBasis::RollingAnnualized)
    );
}

#[test]
fn an_evaluation_the_schema_cannot_store_is_unusable_never_converted() {
    let inputs = two_lines();
    let ids: Vec<Uuid> = inputs.lines.iter().map(|l| l.line_id).collect();
    let check = |mutate: &dyn Fn(&mut OrderEvaluation), expected: fn(&TotalsError) -> bool| {
        let mut e = evaluation(&inputs.lines);
        mutate(&mut e);
        let error = total_rows(&e, &ids, "USD").unwrap_err();
        assert!(expected(&error), "{error:?}");
    };
    // D-216: only a fraction without a terminating decimal expansion has no canonical text.
    check(&|e| e.lines[0].totals.gross.exact = money("1", "3"), |e| {
        matches!(e, TotalsError::Fraction(_))
    });
    check(&|e| e.order_totals.net.exact = money("1000", "7"), |e| {
        matches!(e, TotalsError::Fraction(_))
    });
    // Beyond the schema's 28 significant digits.
    check(
        &|e| e.lines[0].totals.gross.exact = money(&"9".repeat(29), "1"),
        |e| matches!(e, TotalsError::Fraction(_)),
    );
    // A display finer than the currency scale is never rounded.
    check(
        &|e| e.lines[1].totals.gross.display = "10.001".into(),
        |e| matches!(e, TotalsError::Display(_)),
    );
    check(&|e| e.lines[1].totals.gross.display = "1e1".into(), |e| {
        matches!(e, TotalsError::Display(_))
    });
    check(&|e| e.lines[0].declaration = None, |e| {
        matches!(e, TotalsError::Undeclared(_))
    });
    check(&|e| e.order_declaration = None, |e| {
        matches!(e, TotalsError::Undeclared(_))
    });
    check(&|e| e.tcv.as_mut().unwrap().basis = None, |e| {
        matches!(e, TotalsError::Undeclared(_))
    });
    check(&|e| e.tcv = None, |e| {
        matches!(e, TotalsError::Undeclared(_))
    });
    check(
        &|e| {
            e.order_declaration = Some(declaration(EvaluationChargeKind::OneTime));
        },
        |e| matches!(e, TotalsError::Undeclared(_)),
    );
    // Usage charges need the usage declaration, and a usage declaration needs usage charges.
    check(&|e| e.lines[0].usage_flagged = true, |e| {
        matches!(e, TotalsError::Usage(Some(_)))
    });
    check(
        &|e| e.lines[0].usage_declaration = Some(declaration(EvaluationChargeKind::Usage)),
        |e| matches!(e, TotalsError::Usage(Some(_))),
    );
    check(
        &|e| {
            e.lines[0].usage_flagged = true;
            e.lines[0].usage_declaration = Some(declaration(EvaluationChargeKind::Usage));
        },
        |e| matches!(e, TotalsError::Usage(None)),
    );
    check(
        &|e| e.order_usage_declaration = Some(declaration(EvaluationChargeKind::Usage)),
        |e| matches!(e, TotalsError::Usage(None)),
    );
    check(
        &|e| {
            e.lines[0].usage_flagged = true;
            e.lines[0].usage_declaration = Some(declaration(EvaluationChargeKind::Recurring));
            e.order_usage_declaration = Some(declaration(EvaluationChargeKind::Usage));
        },
        |e| matches!(e, TotalsError::Usage(Some(_))),
    );
    check(
        &|e| {
            e.lines.pop();
        },
        |e| matches!(e, TotalsError::Coverage),
    );
    check(
        &|e| e.lines[0].totals.gross.exact = ExactMoney::new("10", "1", "EUR", 2).unwrap(),
        |e| matches!(e, TotalsError::Currency(_)),
    );
}

/// DESIGN §3.7: a usage-bearing line stores its committed component and a usage row with
/// `uncommitted_usage`, NULL amounts and the producer's usage declarations (exclusions included)
/// verbatim; the roll-up has its own usage row; the TCV stays on the order/recurring row only.
#[test]
fn a_usage_bearing_line_stores_an_uncommitted_usage_row_beside_its_committed_component() {
    let inputs = two_lines();
    let ids: Vec<Uuid> = inputs.lines.iter().map(|l| l.line_id).collect();
    let mut e = evaluation(&inputs.lines);
    let mut usage = declaration(EvaluationChargeKind::Usage);
    usage.exclusions = serde_json::json!({"usage": "metered, billed in arrears"});
    e.lines[1].usage_flagged = true;
    e.lines[1].usage_declaration = Some(usage.clone());
    e.order_usage_declaration = Some(usage.clone());
    let rows = total_rows(&e, &ids, "USD").unwrap();
    let kinds: Vec<(TotalScope, Uuid, EvaluationChargeKind, AmountStatus)> = rows
        .iter()
        .map(|r| (r.scope, r.line_id, r.charge_kind, r.amount_status))
        .collect();
    assert_eq!(
        kinds,
        [
            (
                TotalScope::Line,
                ids[0],
                EvaluationChargeKind::Recurring,
                AmountStatus::Committed
            ),
            (
                TotalScope::Line,
                ids[1],
                EvaluationChargeKind::Recurring,
                AmountStatus::Committed
            ),
            (
                TotalScope::Line,
                ids[1],
                EvaluationChargeKind::Usage,
                AmountStatus::UncommittedUsage
            ),
            (
                TotalScope::Order,
                Uuid::nil(),
                EvaluationChargeKind::Recurring,
                AmountStatus::Committed
            ),
            (
                TotalScope::Order,
                Uuid::nil(),
                EvaluationChargeKind::Usage,
                AmountStatus::UncommittedUsage
            ),
        ]
    );
    for row in rows
        .iter()
        .filter(|r| r.charge_kind == EvaluationChargeKind::Usage)
    {
        assert_eq!((&row.gross, &row.net, &row.discount), (&None, &None, &None));
        assert!(row.tcv.is_none());
        assert_eq!(row.exclusions, usage.exclusions);
        assert_eq!(row.amount_basis, usage.amount_basis);
    }
    assert!(
        rows[3].tcv.is_some(),
        "the TCV rides on the order/recurring row only"
    );
}

/// D-216/D-213: a terminating producer fraction is stored as its canonical decimal text (cents
/// included), a display estimate at the currency scale in canonical spelling; both values are
/// the producer's exactly. Nothing is rounded or summed.
#[test]
fn producer_fractions_and_displays_are_stored_as_canonical_decimal_text() {
    let inputs = two_lines();
    let ids: Vec<Uuid> = inputs.lines.iter().map(|l| l.line_id).collect();
    let mut e = evaluation(&inputs.lines);
    e.lines[0].totals.gross = Amount {
        exact: money("21", "2"),
        display: "10.50".into(),
    };
    e.lines[0].totals.net = Amount {
        exact: money("-1", "8"),
        display: "-0.13".into(),
    };
    e.order_totals.gross.exact = money("21", "2");
    let rows = total_rows(&e, &ids, "USD").unwrap();
    let pair = |a: &str, b: &str| Some((a.to_owned(), b.to_owned()));
    assert_eq!(rows[0].gross, pair("10.5", "10.5"));
    assert_eq!(rows[0].net, pair("-0.125", "-0.13"));
    assert_eq!(
        rows[2].gross.as_ref().unwrap().0,
        "10.5",
        "the order roll-up fraction renders the same way"
    );
    for row in &rows {
        for (exact, display) in [row.gross.as_ref().unwrap(), row.net.as_ref().unwrap()] {
            assert!(decimal_valid(exact, 28) && decimal_valid(display, 2));
        }
    }
}

/// D-216 one currency: an evaluation in a currency other than the frozen market's is a producer
/// contract defect, not an unstorable evaluation.
#[test]
fn an_evaluation_outside_the_market_currency_is_a_contract_defect() {
    let inputs = two_lines();
    let ids: Vec<Uuid> = inputs.lines.iter().map(|l| l.line_id).collect();
    let error = total_rows(&evaluation(&inputs.lines), &ids, "EUR").unwrap_err();
    assert_eq!(error, TotalsError::MarketCurrency);
    assert!(error.is_contract_defect());
    assert!(!TotalsError::Fraction("x").is_contract_defect());
}

/// `decimal_valid` agrees with `bss_orders__decimal_valid` (07 integrity) case by case.
#[test]
fn the_decimal_rule_mirrors_the_schema_function() {
    // The Postgres tier keeps a copy of this table (`decimal_rule_agrees_with_the_schema_function`
    // in `src/infra/storage/tests/constraints.rs`): change both together.
    for (value, scale, valid) in [
        ("0", 2, true),
        ("10", 2, true),
        ("10.5", 2, true),
        ("-10.25", 2, true),
        ("10.00", 2, false),
        ("10.123", 2, false),
        ("010", 2, false),
        ("-0", 2, false),
        ("+1", 2, false),
        ("1e3", 2, false),
        (".5", 2, false),
        ("1.", 2, false),
        ("", 2, false),
        ("1234567890123456789012345678", 0, true),
        ("12345678901234567890123456789", 0, false),
        ("0.0000000000000000000000000001", 28, true),
    ] {
        assert_eq!(decimal_valid(value, scale), valid, "{value} at {scale}");
    }
}

/// The 200/201-line authoring bound and the 1 MiB whole-version bound, measured through the
/// production serializers (`OrderPin::new`/`encode`).
#[test]
fn the_line_and_version_size_boundaries() {
    assert_eq!(line_cap_verdict(200, 200), GuardVerdict::Pass);
    assert_eq!(
        line_cap_verdict(201, 200),
        GuardVerdict::Fail(Reason::LineCapExceeded)
    );
    // 200 lines, each with its encoded stub pin, fit the version.
    let lines: Vec<BasketLine> = (1..=200)
        .map(|n| basket_line(n, vec![item(ITEM, "1", None)]))
        .collect();
    let inputs = Inputs::new(lines);
    let pins: Vec<usize> = inputs
        .lines
        .iter()
        .map(|l| {
            let receipt = stub_receipt(&inputs, l.line_id, CANDIDATE);
            OrderPin::new(
                AssessmentRef {
                    assessment_id: Uuid::from_u128(70),
                    assessed_at: anchor_at(),
                    resolve_date: day(),
                },
                AcceptedVersionRef {
                    order_id: ORDER,
                    order_version: OrderVersion::try_from(i64::from(CANDIDATE)).unwrap(),
                    line_id: l.line_id,
                },
                receipt,
            )
            .unwrap()
            .encode()
            .unwrap()
            .len()
        })
        .collect();
    // The whole version: the pins, every totals row and the version row, each counted column
    // by column by the production accounting (`TotalRow::persisted_bytes`, `version_row_bytes`).
    let ids: Vec<Uuid> = inputs.lines.iter().map(|l| l.line_id).collect();
    let version = VersionContribution {
        candidate: CANDIDATE,
        payer_tenant_id: PAYER,
        category: "new_sale".into(),
        contract_id: None,
        market_currency: Some("USD".into()),
        market_region: Some("eu".into()),
        amendment_reason: None,
    };
    let version_row = version_row_bytes(&version, "submit");
    assert_eq!(version_row, 16 + 4 + 4 + 3 + 2 + 16 + 8 + 36 + 16 + 6 + 8);
    let persisted = |e: &OrderEvaluation| -> Vec<usize> {
        let rows = total_rows(e, &ids, "USD").unwrap();
        pins.iter()
            .copied()
            .chain(rows.iter().map(TotalRow::persisted_bytes))
            .chain([version_row])
            .collect()
    };
    let base = evaluation(&inputs.lines);
    let total: usize = persisted(&base).iter().sum();
    assert!(total < MAX_VERSION_BYTES, "{total}");
    assert_eq!(version_capacity(persisted(&base)), GuardVerdict::Pass);
    // A producer item breakdown sized so the version is exactly at the limit fits; one more
    // byte refuses before the commercial commit. `[]` (2 bytes) becomes a string of n + 2.
    let padded = |n: usize| {
        let mut e = base.clone();
        e.order_declaration.as_mut().unwrap().item_breakdown =
            serde_json::Value::String("x".repeat(n));
        e
    };
    let room = MAX_VERSION_BYTES - total;
    assert_eq!(
        persisted(&padded(room)).iter().sum::<usize>(),
        MAX_VERSION_BYTES
    );
    assert_eq!(
        version_capacity(persisted(&padded(room))),
        GuardVerdict::Pass
    );
    assert_eq!(
        version_capacity(persisted(&padded(room + 1))),
        GuardVerdict::Fail(Reason::PurchaseCapacityExceeded)
    );
    // Exactly at the limit fits; one byte over refuses before the commercial commit.
    assert_eq!(version_capacity([MAX_VERSION_BYTES]), GuardVerdict::Pass);
    assert_eq!(
        version_capacity([MAX_VERSION_BYTES - 10, 11]),
        GuardVerdict::Fail(Reason::PurchaseCapacityExceeded)
    );
    assert_eq!(
        version_capacity([usize::MAX, 1]),
        GuardVerdict::Fail(Reason::PurchaseCapacityExceeded),
        "never wraps"
    );
}

/// D-188 candidate exhaustion is the canonical resource-exhaustion Problem (429) with the quota
/// subject `version-allocation`, unsettled: an engine error mapping, not a registered reason.
#[test]
fn candidate_exhaustion_is_the_canonical_resource_exhausted_problem() {
    let bss_orders_sdk::OrdersError::Other { canonical } =
        bss_orders_sdk::OrdersError::from(EngineError::VersionAllocationExhausted)
    else {
        panic!("not a canonical error")
    };
    let problem = toolkit_canonical_errors::Problem::from(canonical);
    assert_eq!(
        problem.problem_type,
        "gts://gts.cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~"
    );
    assert_eq!(problem.status, Some(429));
    let violations = problem.context["violations"].as_array().unwrap();
    assert_eq!(violations.len(), 1);
    assert_eq!(violations[0]["subject"], "version-allocation");
    assert!(violations[0].get("retry_after_seconds").is_none());
}

/// Pricing's acceptance command is issued only by submit phase B (`infra/gate/submit.rs`) and
/// implemented only by the development double (`infra/gate/stubs.rs`): no other production
/// source names `SellabilityV1::check` or calls a `.check(`.
#[test]
fn only_phase_b_and_the_stub_name_the_acceptance_check() {
    fn files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = Vec::new();
    files(&root.join("src"), &mut sources);
    let mut callers = Vec::new();
    for file in sources {
        let relative = file
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if relative.contains("/tests/") || relative.ends_with("/tests.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap();
        if text.contains(".check(")
            || text.contains("SellabilityV1::check")
            || text.contains("impl SellabilityV1 for")
        {
            callers.push(relative);
        }
    }
    callers.sort();
    assert_eq!(
        callers,
        ["src/infra/gate/stubs.rs", "src/infra/gate/submit.rs"],
        "the acceptance check is named outside phase B and the stub"
    );
}

/// The order cap counts every selected item across all lines, before deduplication.
#[test]
fn consumed_item_capacity_counts_the_entire_order() {
    use bss_orders::domain::gate::commercial::consumed_item_capacity;
    for counts in [vec![200], vec![100, 100], vec![1; 200], vec![]] {
        assert_eq!(consumed_item_capacity(counts), GuardVerdict::Pass);
    }
    for counts in [
        vec![201],
        vec![100, 100, 1],
        vec![1; 201],
        vec![usize::MAX, 1],
    ] {
        assert_eq!(
            consumed_item_capacity(counts),
            GuardVerdict::Fail(Reason::PurchaseCapacityExceeded)
        );
    }
}

#[test]
fn non_recurring_requests_freeze_null_keys_and_mixed_orders_claim_only_recurring_lines() {
    use bss_orders::domain::overlap::{
        ClaimDirective, ClaimParties, OverlapScopeKey, ProposedClaims, plan,
    };
    use bss_orders_sdk::catalog::{OrderState, Trigger};
    for kind in [ChargeKind::OneTime, ChargeKind::Usage] {
        for mixed in [false, true] {
            let mut inputs = two_lines();
            let keyless = if mixed {
                vec![line_id(1)]
            } else {
                vec![line_id(1), line_id(2)]
            };
            for id in keyless {
                inputs.keys.remove(&id);
                for cell in &mut Arc::make_mut(inputs.bindings.get_mut(&id).unwrap()).cells {
                    cell.binding.as_mut().unwrap().kind = kind.clone();
                }
            }
            let requests = build_line_requests(&inputs.facts(), CANDIDATE).unwrap();
            let value = line_requests_value(&requests, &frozen_order(&inputs.lines)).unwrap();
            assert!(value[&line_id(1).to_string()]["overlap_scope_key"].is_null());
            assert!(value[&line_id(1).to_string()]["catalog_subscription_product_key"].is_null());
            let frozen = decode_line_requests(&value).unwrap();
            let keys = frozen
                .lines
                .iter()
                .flat_map(|l| l.scopes())
                .map(|k| OverlapScopeKey::try_from(k.to_owned()).unwrap());
            let proposal = ProposedClaims::new(
                ClaimParties {
                    payer_tenant_id: PAYER,
                    resource_tenant_id: RESOURCE,
                },
                keys,
            );
            let directive = ClaimDirective::for_transition(
                Trigger::Submit,
                OrderState::Submitted,
                Some(proposal),
            )
            .unwrap();
            let ClaimDirective::Replace(proposal) = directive else {
                panic!("submit replaces claims")
            };
            let claims = plan(&[], &proposal).unwrap();
            assert_eq!(claims.missing.len(), usize::from(mixed));
            if mixed {
                assert_eq!(
                    claims.missing[0].overlap_scope_key.as_str(),
                    format!("scope-{}", line_id(2))
                );
            }
            assert!(claims.superseded.is_empty());
            let mut missing = value.clone();
            missing[&line_id(1).to_string()]
                .as_object_mut()
                .unwrap()
                .remove("item_keys_v1");
            assert!(decode_line_requests(&missing).is_err());
        }
    }
}

#[test]
fn committed_row_must_equal_the_frozen_query() {
    use bss_orders::domain::gate::commercial::verify_commit_line;
    let inputs = two_lines();
    let frozen = build_line_requests(&inputs.facts(), CANDIDATE).unwrap();
    let query = &frozen[0].query;
    let line = &inputs.lines[0];
    assert!(verify_commit_line(query, ORDER, 2, line));
    assert!(!verify_commit_line(query, Uuid::nil(), 2, line));
    assert!(!verify_commit_line(query, ORDER, 3, line));
    for case in 0..7 {
        let mut row = line.clone();
        match case {
            0 => row.line_id = Uuid::nil(),
            1 => row.catalog.plan_id = Uuid::nil(),
            2 => row.catalog.revision_id = Uuid::nil(),
            3 => row.currency = Some("EUR".into()),
            4 => row.catalog.items[0].quantity = "9".into(),
            5 => row.catalog.items[0].selected_dim_value = Some("different".into()),
            _ => {
                row.catalog.items.pop();
            }
        }
        assert!(!verify_commit_line(query, ORDER, 2, &row), "case {case}");
    }
}

/// Ignoring explicit components would silently store the aggregate under a single kind.
#[test]
fn explicit_components_copy_each_kind_without_splitting_the_preview_aggregate() {
    let inputs = two_lines();
    let mut value = serde_json::to_value(evaluation(&inputs.lines)).unwrap();
    let components = serde_json::json!([
        {"totals": totals("7", "7"), "declaration": declaration(EvaluationChargeKind::Recurring)},
        {"totals": totals("3", "3"), "declaration": declaration(EvaluationChargeKind::OneTime)}
    ]);
    for line in value["lines"].as_array_mut().unwrap() {
        line.as_object_mut().unwrap().remove("declaration");
        line["committed_components"] = components.clone();
    }
    value.as_object_mut().unwrap().remove("order_declaration");
    value["order_committed_components"] = components;
    let e: OrderEvaluation = serde_json::from_value(value).unwrap();
    let ids: Vec<_> = inputs.lines.iter().map(|l| l.line_id).collect();
    let rows = total_rows(&e, &ids, "USD").unwrap();
    assert_eq!(rows.len(), 6);
    for scope_rows in rows.chunks(2) {
        assert_eq!(scope_rows[0].charge_kind, EvaluationChargeKind::Recurring);
        assert_eq!(scope_rows[0].gross, Some(("7".into(), "7".into())));
        assert_eq!(scope_rows[1].charge_kind, EvaluationChargeKind::OneTime);
        assert_eq!(scope_rows[1].gross, Some(("3".into(), "3".into())));
        assert!(scope_rows[1].tcv.is_none());
    }
    assert_eq!(e.order_totals.gross.display, "20");
    assert_eq!(e.lines[0].totals.gross.display, "10");
    assert!(rows[4].tcv.is_some());
}

fn non_recurring_evaluation(usage: bool, carrier: bool) -> OrderEvaluation {
    let inputs = two_lines();
    let mut value = serde_json::to_value(evaluation(&inputs.lines)).unwrap();
    let components = if usage {
        serde_json::json!([])
    } else {
        serde_json::json!([{"totals": totals("10", "10"),
            "declaration": declaration(EvaluationChargeKind::OneTime)}])
    };
    for line in value["lines"].as_array_mut().unwrap() {
        line.as_object_mut().unwrap().remove("declaration");
        line["committed_components"] = components.clone();
        line["usage_flagged"] = usage.into();
        if usage {
            line["usage_declaration"] =
                serde_json::to_value(declaration(EvaluationChargeKind::Usage)).unwrap();
        }
    }
    value.as_object_mut().unwrap().remove("order_declaration");
    value["order_committed_components"] = components;
    if usage {
        value["order_usage_declaration"] =
            serde_json::to_value(declaration(EvaluationChargeKind::Usage)).unwrap();
    }
    if carrier {
        value["order_tcv_carrier"] = serde_json::json!({"totals": totals("0", "0"),
            "declaration": declaration(EvaluationChargeKind::Recurring)});
    }
    serde_json::from_value(value).unwrap()
}

/// Even usage-only input cannot omit both TCV and its explicit storage carrier.
#[test]
fn usage_only_without_tcv_or_carrier_is_unusable() {
    let mut e = non_recurring_evaluation(true, false);
    e.tcv = None;
    let ids: Vec<_> = e.lines.iter().map(|l| l.line_id).collect();
    assert!(total_rows(&e, &ids, "USD").is_err());
}

#[test]
fn non_recurring_baskets_copy_only_the_producers_explicit_tcv_carrier() {
    for usage in [false, true] {
        let mut e = non_recurring_evaluation(usage, true);
        if usage {
            e.tcv.as_mut().unwrap().exact = money("0", "1");
            e.tcv.as_mut().unwrap().display = "0".into();
        }
        let ids: Vec<_> = e.lines.iter().map(|l| l.line_id).collect();
        let frozen = serde_json::to_vec(&e).unwrap();
        let decoded: OrderEvaluation = serde_json::from_slice(&frozen).unwrap();
        assert_eq!(e, decoded);
        let rows = total_rows(&decoded, &ids, "USD").unwrap();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows.iter().filter(|r| r.tcv.is_some()).count(), 1);
        let carrier = rows.iter().find(|r| r.tcv.is_some()).unwrap();
        assert_eq!(
            (carrier.scope, carrier.charge_kind),
            (TotalScope::Order, EvaluationChargeKind::Recurring)
        );
        assert_eq!(carrier.gross, Some(("0".into(), "0".into())));
        for line in rows.iter().filter(|r| r.scope == TotalScope::Line) {
            assert_eq!(
                line.charge_kind,
                if usage {
                    EvaluationChargeKind::Usage
                } else {
                    EvaluationChargeKind::OneTime
                }
            );
            assert_eq!(line.gross.is_none(), usage);
        }
        let mut missing = serde_json::to_value(&decoded).unwrap();
        missing.as_object_mut().unwrap().remove("order_tcv_carrier");
        let missing: OrderEvaluation = serde_json::from_value(missing).unwrap();
        assert!(total_rows(&missing, &ids, "USD").is_err());
    }
}

#[test]
fn explicit_components_reject_duplicates_conflicts_missing_declarations_and_currency_mismatch() {
    let base = non_recurring_evaluation(false, true);
    let ids: Vec<_> = base.lines.iter().map(|l| l.line_id).collect();
    let value = serde_json::to_value(&base).unwrap();
    for case in 0..9 {
        let mut invalid = value.clone();
        match case {
            0 => {
                let c = invalid["lines"][0]["committed_components"][0].clone();
                invalid["lines"][0]["committed_components"]
                    .as_array_mut()
                    .unwrap()
                    .push(c);
            }
            1 => {
                let c = invalid["order_committed_components"][0].clone();
                invalid["order_committed_components"]
                    .as_array_mut()
                    .unwrap()
                    .push(c);
            }
            2 => {
                invalid["lines"][0]["declaration"] =
                    serde_json::to_value(declaration(EvaluationChargeKind::Recurring)).unwrap();
            }
            3 => invalid["order_tcv_carrier"]["declaration"]["charge_kind"] = "one_time".into(),
            4 => {
                invalid["lines"][0]["committed_components"][0]["totals"]["net"]["exact"]["currency"] =
                    "EUR".into();
            }
            5 => {
                invalid["order_tcv_carrier"]["totals"]["gross"]["exact"]["currency_scale"] =
                    3.into();
            }
            6 => {
                invalid["lines"][0]["committed_components"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("declaration");
            }
            7 => {
                // A legacy alias must declare the very same amount, not the aggregate of other components.
                invalid["lines"][0]["declaration"] =
                    serde_json::to_value(declaration(EvaluationChargeKind::OneTime)).unwrap();
                invalid["lines"][0]["committed_components"][0]["totals"] =
                    serde_json::to_value(totals("9", "9")).unwrap();
            }
            _ => invalid["order_committed_components"] = serde_json::json!([]),
        }
        match serde_json::from_value::<OrderEvaluation>(invalid) {
            Ok(e) => assert!(total_rows(&e, &ids, "USD").is_err(), "case {case}"),
            Err(_) => assert_eq!(case, 6, "case {case} must reach validation"),
        }
    }
    // A missing list uses the legacy path; explicit [] never falls back to aggregate money.
    let mut usage = serde_json::to_value(non_recurring_evaluation(true, true)).unwrap();
    usage["lines"][0]
        .as_object_mut()
        .unwrap()
        .remove("committed_components");
    let usage: OrderEvaluation = serde_json::from_value(usage).unwrap();
    assert!(matches!(
        total_rows(&usage, &ids, "USD"),
        Err(TotalsError::Undeclared(_))
    ));
    // A second recurring declaration would conflict with the carrier even if amounts match.
    let mut conflicting = value;
    conflicting["order_committed_components"][0]["declaration"]["charge_kind"] = "recurring".into();
    let conflicting: OrderEvaluation = serde_json::from_value(conflicting).unwrap();
    assert!(matches!(
        total_rows(&conflicting, &ids, "USD"),
        Err(TotalsError::Components)
    ));
}

#[test]
fn explicit_component_evidence_counts_toward_persisted_bytes() {
    let mut e = non_recurring_evaluation(false, true);
    let ids: Vec<_> = e.lines.iter().map(|l| l.line_id).collect();
    let before = total_rows(&e, &ids, "USD").unwrap();
    let frozen = serde_json::to_value(&e).unwrap();
    assert_eq!(
        frozen["order_tcv_carrier"]["totals"]["gross"]["exact"]["numerator"],
        "0"
    );
    assert_eq!(
        frozen["lines"][0]["committed_components"][0]["declaration"]["charge_kind"],
        "one_time"
    );
    e.order_tcv_carrier.as_mut().unwrap().declaration.exclusions =
        serde_json::json!({"usage": "excluded"});
    let after = total_rows(&e, &ids, "USD").unwrap();
    // {} -> {"usage": "excluded"} is 18 additional jsonb text bytes.
    assert_eq!(
        after.iter().map(TotalRow::persisted_bytes).sum::<usize>()
            - before.iter().map(TotalRow::persisted_bytes).sum::<usize>(),
        18
    );
    assert_eq!(
        after.last().unwrap().exclusions,
        serde_json::json!({"usage": "excluded"})
    );
}

#[test]
fn explicit_components_cannot_hide_foreign_currency_in_the_preview_aggregate() {
    for order in [false, true] {
        let mut e = non_recurring_evaluation(false, true);
        if order {
            let net = &e.order_totals.net.exact;
            e.order_totals.net.exact = ExactMoney::new(
                net.numerator(),
                net.denominator(),
                "EUR",
                net.currency_scale(),
            )
            .unwrap();
        } else {
            let gross = &e.lines[0].totals.gross.exact;
            e.lines[0].totals.gross.exact =
                ExactMoney::new(gross.numerator(), gross.denominator(), gross.currency(), 3)
                    .unwrap();
        }
        let ids: Vec<_> = e.lines.iter().map(|l| l.line_id).collect();
        assert!(matches!(
            total_rows(&e, &ids, "USD"),
            Err(TotalsError::Currency(_))
        ));
    }
}

#[test]
fn complete_frozen_evidence_requires_all_receipt_selections_and_versions() {
    let inputs = two_lines();
    let requests = build_line_requests(&inputs.facts(), CANDIDATE).unwrap();
    let value = line_requests_value(&requests, &frozen_order(&inputs.lines)).unwrap();
    let decoded = decode_line_requests(&value).unwrap();
    for line in &decoded.lines {
        let receipt = bss_orders::domain::gate::commercial::deterministic_receipt(
            line.query.clone(),
            &inputs.bindings[&line.line_id],
            Uuid::new_v4(),
            anchor_at(),
            600,
        )
        .unwrap();
        assert!(line.matches_bindings(&receipt.bindings));
        let mut wrong_version = receipt.bindings.clone();
        wrong_version[0].sku_version += 1;
        assert!(!line.matches_bindings(&wrong_version));
        let mut missing = line.clone();
        missing.overlap_item_keys.as_mut().unwrap().pop();
        assert!(!missing.matches_bindings(&receipt.bindings));
        let mut mixed = line.clone();
        mixed.overlap_scope_key = Some("scope".into());
        assert!(!mixed.matches_bindings(&receipt.bindings));
        let mut legacy = line.clone();
        legacy.overlap_item_keys = None;
        legacy.overlap_scope_key = Some("legacy-scope".into());
        assert_eq!(
            legacy.matches_bindings(&receipt.bindings),
            receipt.bindings.len() == 1
        );
    }
    let id = line_id(1).to_string();
    let mut duplicate = value.clone();
    let items = duplicate[&id]["item_keys_v1"].as_array_mut().unwrap();
    items.push(items[0].clone());
    assert!(decode_line_requests(&duplicate).is_err());
    let mut mixed = value.clone();
    mixed[&id]["overlap_scope_key"] = serde_json::json!(null);
    assert!(decode_line_requests(&mixed).is_err());
    let mut unknown = value;
    unknown[&id]["item_keys_v1"][0]["classification"]["extra"] = serde_json::json!("forbidden");
    assert!(decode_line_requests(&unknown).is_err());
}

#[test]
fn frozen_item_identity_preserves_requested_dimension_for_default_binding() {
    let mut inputs = two_lines();
    let id = inputs.lines[0].line_id;
    inputs.lines[0].catalog.items[0].selected_dim_value = Some("requested-region".into());
    let cell = &mut Arc::make_mut(inputs.bindings.get_mut(&id).unwrap()).cells[0];
    cell.selection.dimension_value = Some("requested-region".into());
    // Pricing preserves the requested dimension in AcceptedBinding, even when its chain
    // resolved through the default; via_default records that resolution choice.
    cell.binding.as_mut().unwrap().dimension_key = Some("location".into());
    cell.binding.as_mut().unwrap().dimension_value = Some("requested-region".into());
    cell.binding.as_mut().unwrap().via_default = true;
    inputs.keys.get_mut(&id).unwrap()[0]
        .item_key
        .as_mut()
        .unwrap()
        .selection
        .dimension_value = Some("requested-region".into());
    let requests = build_line_requests(&inputs.facts(), CANDIDATE).unwrap();
    let request = requests.iter().find(|r| r.line_id == id).unwrap();
    assert_eq!(
        request.overlap_item_keys.as_ref().unwrap()[0]
            .selection
            .dimension_value
            .as_deref(),
        Some("requested-region")
    );
    let receipt = bss_orders::domain::gate::commercial::deterministic_receipt(
        request.query.clone(),
        &inputs.bindings[&id],
        Uuid::new_v4(),
        anchor_at(),
        600,
    )
    .unwrap();
    let frozen = decode_line_requests(
        &line_requests_value(&requests, &frozen_order(&inputs.lines)).unwrap(),
    )
    .unwrap();
    assert!(
        frozen
            .lines
            .iter()
            .find(|l| l.line_id == id)
            .unwrap()
            .matches_bindings(&receipt.bindings)
    );
}

#[test]
fn stored_query_decode_failure_keeps_the_codec_kind() {
    let inputs = two_lines();
    let requests = build_line_requests(&inputs.facts(), CANDIDATE).unwrap();
    let mut value = line_requests_value(&requests, &frozen_order(&inputs.lines)).unwrap();
    value[line_id(1).to_string()]["query"] = serde_json::json!({"line_id": 7});
    let err = decode_line_requests(&value).unwrap_err();
    assert_eq!(err.member, "query");
    assert_eq!(err.codec, Some(CodecError::Malformed(MalformedKind::Data)));
    assert!(err.to_string().contains("query ("), "{err}");
}

#[test]
fn stored_receipt_decode_failure_keeps_the_codec_kind() {
    let err = decode_receipt_result(&serde_json::json!({"accepted": {"bad": 1}})).unwrap_err();
    assert_eq!(err.member, "receipt");
    assert_eq!(err.codec, Some(CodecError::Malformed(MalformedKind::Data)));
}

/// An attempt `line_requests` value written before the digest and the report were typed: the
/// untyped codec's exact bytes for [`two_lines`] at candidate 2 with [`frozen_report`] stored
/// as `serde_json::to_value(GateReport)`.
///
/// The fixture is frozen: it was captured from the pre-C4 encoding and must never be
/// regenerated from head, or it stops proving that old rows still decode and new rows still
/// write the same bytes.
const LEGACY_ATTEMPT: &str = include_str!("fixtures/frozen-attempt-legacy-v1.json");

#[test]
fn frozen_order_decodes_a_report_written_as_value() {
    let value: serde_json::Value = serde_json::from_str(LEGACY_ATTEMPT).unwrap();
    let decoded = decode_line_requests(&value).unwrap();
    assert_eq!(decoded.order.report, frozen_report());
    assert_eq!(
        decoded.order.report.outcomes[1].catalog_scope_key(),
        Some("eu")
    );
    // A report that is not a gate report is unreadable evidence, never committed from.
    let mut v = value;
    v[ORDER_MEMBER]["report"]["failures"] = serde_json::json!("none");
    assert_eq!(
        decode_line_requests(&v).map(|_| ()),
        Err(bss_orders::domain::gate::commercial::MalformedRequests::member("order"))
    );
}

/// D-242: an attempt frozen before D-241 may hold an exact pair with a common factor, which
/// Orders then accepted and reduced when rendering totals. The stored decode reduces it, so the
/// attempt still recovers with the same totals instead of failing as `Integration` on every
/// retry; a part that was never canonical still refuses, and the wire stays strict.
#[test]
fn a_stored_unreduced_exact_pair_decodes_reduced() {
    let reference = decode_line_requests(&serde_json::from_str(LEGACY_ATTEMPT).unwrap()).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(LEGACY_ATTEMPT).unwrap();
    let evaluation = &mut value[ORDER_MEMBER]["evaluation"];
    evaluation["order_totals"]["gross"]["exact"]["numerator"] = serde_json::json!("40");
    evaluation["order_totals"]["gross"]["exact"]["denominator"] = serde_json::json!("2");
    evaluation["lines"][0]["totals"]["net"]["exact"]["numerator"] = serde_json::json!("30");
    evaluation["lines"][0]["totals"]["net"]["exact"]["denominator"] = serde_json::json!("3");
    evaluation["tcv"]["exact"]["numerator"] = serde_json::json!("480");
    evaluation["tcv"]["exact"]["denominator"] = serde_json::json!("2");
    let decoded = decode_line_requests(&value).unwrap();
    assert_eq!(decoded.order.evaluation, reference.order.evaluation);
    assert_eq!(
        decoded
            .order
            .evaluation
            .order_totals
            .gross
            .exact
            .numerator(),
        "20"
    );
    assert_eq!(
        decoded
            .order
            .evaluation
            .order_totals
            .gross
            .exact
            .denominator(),
        "1"
    );
    // A part that is not canonical integer text is not a legacy pair: still unreadable evidence.
    value[ORDER_MEMBER]["evaluation"]["order_totals"]["gross"]["exact"]["numerator"] =
        serde_json::json!("040");
    assert_eq!(
        decode_line_requests(&value).map(|_| ()),
        Err(bss_orders::domain::gate::commercial::MalformedRequests::member("order"))
    );
    // The port wire stays strict (money ADR rule 6, D-241 item 2).
    assert!(
        serde_json::from_value::<ExactMoney>(serde_json::json!({
            "numerator": "40", "denominator": "2", "currency": "USD", "currency_scale": 2
        }))
        .is_err()
    );
}

/// D-242: the stored decode reduces only the typed `ExactMoney` positions of the evaluation.
/// Producer evidence (`TotalsDeclaration`'s opaque members, `Tcv.evidence`) is copied verbatim
/// into `resolved_total` rows (D-197/D-216), so a money-shaped unreduced object there keeps its
/// stored bytes, while unreduced typed pairs (a negative discount, the TCV carrier) are reduced.
#[test]
fn the_stored_decode_reduces_only_typed_exact_positions() {
    let evidence = serde_json::json!({
        "numerator": "40", "denominator": "2", "currency": "USD", "currency_scale": 2
    });
    let unreduced = |numerator: &str| {
        serde_json::json!({
            "numerator": numerator, "denominator": "2", "currency": "USD", "currency_scale": 2
        })
    };
    let mut value: serde_json::Value = serde_json::from_str(LEGACY_ATTEMPT).unwrap();
    let evaluation = &mut value[ORDER_MEMBER]["evaluation"];
    evaluation["lines"][0]["declaration"]["item_breakdown"] = serde_json::json!([evidence]);
    evaluation["tcv"]["evidence"]["carry"] = evidence.clone();
    evaluation["tcv"]["exact"] = unreduced("480");
    evaluation["lines"][0]["totals"]["discount"] =
        serde_json::json!({"status": "available", "exact": unreduced("-40"), "display": "-20"});
    let mut carrier_totals = evaluation["order_totals"].clone();
    carrier_totals["gross"]["exact"] = unreduced("40");
    evaluation["order_tcv_carrier"] = serde_json::json!({
        "totals": carrier_totals,
        "declaration": evaluation["order_declaration"].clone(),
    });
    let decoded = decode_line_requests(&value).unwrap().order.evaluation;
    let line = &decoded.lines[0];
    assert_eq!(
        line.declaration.as_ref().unwrap().item_breakdown,
        serde_json::json!([evidence]),
        "evidence under a declaration keeps its stored bytes"
    );
    let tcv = decoded.tcv.as_ref().unwrap();
    assert_eq!(
        tcv.evidence.as_ref().unwrap()["carry"],
        evidence,
        "TCV evidence is verbatim"
    );
    assert_eq!(
        (tcv.exact.numerator(), tcv.exact.denominator()),
        ("240", "1")
    );
    let Availability::Available(discount) = &line.totals.discount else {
        panic!("the discount is available")
    };
    assert_eq!(
        (discount.exact.numerator(), discount.exact.denominator()),
        ("-20", "1")
    );
    let carrier = &decoded
        .order_tcv_carrier
        .as_ref()
        .unwrap()
        .totals
        .gross
        .exact;
    assert_eq!((carrier.numerator(), carrier.denominator()), ("20", "1"));
}

#[test]
fn line_requests_value_is_byte_stable_with_typed_digest() {
    let inputs = two_lines();
    let requests = build_line_requests(&inputs.facts(), CANDIDATE).unwrap();
    for r in &requests {
        assert_eq!(r.request_digest, request_digest(&r.query), "stored unhexed");
    }
    // The value is stored as JSONB and its map order depends on whether host feature
    // unification enables `serde_json/preserve_order`, so normalize with `sort_all_objects`
    // (as the replay path in `src/api/rest/capture.rs` does) before comparing bytes. Under the
    // default build the sort is a no-op, so this still pins the exact stored bytes, including the
    // lowercase hex spelling of every `request_digest`.
    let canonical_bytes = |mut value: serde_json::Value| {
        value.sort_all_objects();
        serde_json::to_vec(&value).unwrap()
    };
    let bytes =
        canonical_bytes(line_requests_value(&requests, &frozen_order(&inputs.lines)).unwrap());
    assert_eq!(
        String::from_utf8(bytes.clone()).unwrap(),
        LEGACY_ATTEMPT,
        "a new attempt writes the bytes the untyped codec wrote"
    );
    // Encode, decode, re-encode: the same value and the same bytes.
    let decoded = decode_line_requests(&serde_json::from_slice(&bytes).unwrap()).unwrap();
    let again: Vec<LineRequest> = decoded
        .lines
        .iter()
        .map(|l| LineRequest {
            line_id: l.line_id,
            key: l.key.clone(),
            query: l.query.clone(),
            request_digest: l.request_digest,
            overlap_scope_key: l.overlap_scope_key.clone(),
            catalog_subscription_product_key: l.catalog_subscription_product_key.clone(),
            overlap_item_keys: l.overlap_item_keys.clone(),
        })
        .collect();
    assert_eq!(
        canonical_bytes(line_requests_value(&again, &decoded.order).unwrap()),
        bytes
    );
}

#[test]
fn recorded_refusals_are_sale_then_terms() {
    use CommercialReason::*;
    let sale = [
        NotSellable,
        ResolutionChanged,
        ActivationOutsideAcceptedWindow,
        HoldExpired,
        PriceClosed,
        SkuRetired,
        MarketChanged,
    ];
    assert_eq!(RECORDED_REFUSALS[..7], sale);
    assert_eq!(RECORDED_REFUSALS[7..], RECORDED_TERMS_REFUSALS);
    let distinct: std::collections::BTreeSet<&str> =
        RECORDED_REFUSALS.iter().map(|r| r.code()).collect();
    assert_eq!(distinct.len(), RECORDED_REFUSALS.len(), "no reason twice");
    for reason in CONTRACT_DEFECT_REFUSALS {
        assert!(!RECORDED_REFUSALS.contains(&reason), "{reason:?}");
    }
}

#[test]
fn command_key_has_the_frozen_format() {
    const ORDER: Uuid = Uuid::from_u128(0x0a11_ce00_0000_4000_8000_0000_0000_0001);
    const LINE: Uuid = Uuid::from_u128(0x0b0b_0000_0000_4000_8000_0000_0000_0002);
    assert_eq!(
        command_key(ORDER, 3, LINE),
        format!("{ORDER}/3/{LINE}/{SUBMIT_COMMAND}")
    );
    assert_eq!(
        command_key(ORDER, 3, LINE),
        "0a11ce00-0000-4000-8000-000000000001/3/0b0b0000-0000-4000-8000-000000000002/submit"
    );
}
