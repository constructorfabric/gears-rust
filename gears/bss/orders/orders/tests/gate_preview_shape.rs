#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a failed setup or fixture must stop the test at once; `allow`, not `expect`, because not every item unwraps or expects"
)]
//! The Preview result shaping (S3-11; F03 §2.2 step 6, §4.6; D-125, D-216): the TCV is copied
//! only when every line authored term and cycle and is otherwise withheld with the registered
//! reason and every lacking line; deferral and expected fulfillment time follow the resolved
//! service activation dates; every money value is the producer's text, copied verbatim. Also the
//! grep pin that Preview never reaches the submit phase-B sellability check, and the per-caller
//! limit configuration.
use bss_orders::config::GateConfig;
use bss_orders::domain::dates::{
    DateBasis, DatePolicySnapshot, PLATFORM_DEFAULT_POLICY_ID, PolicyRow,
};
use bss_orders::domain::gate::answers::{
    Amount, Availability, ExactMoney, IndicativeTax, LineAmount, LineEvaluation, OrderEvaluation,
    Tcv, Totals,
};
use bss_orders::domain::gate::assess::{Assessment, AssessmentInput, BasketLine, assess};
use bss_orders::domain::gate::ports::{PortAnswer, Unavailable};
use bss_orders::domain::gate::predicates::PredicateId;
use bss_orders::domain::gate::pricing_map::{LineCatalogRequest, PinOutcome, SelectedItem};
use bss_orders::domain::gate::report::{
    PreviewAnswers, PreviewLineFacts, TCV_WITHHELD_REASON, preview_result,
};
use bss_orders::domain::gate::vector::{Applicability, Operation, Vector};
use bss_orders_sdk::authoring::CalendarDate;
use bss_orders_sdk::catalog::Reason;
use bss_orders_sdk::preview::{DiscountView, PinOutcomeView};
use bss_pricing_sdk::read::RevisionRef;
use std::collections::BTreeMap;
use time::{Date, Month};
use uuid::Uuid;

const RUN: Uuid = Uuid::from_u128(0x5a);
const PLAN: Uuid = Uuid::from_u128(1);
const REV: Uuid = Uuid::from_u128(2);
const ITEM: Uuid = Uuid::from_u128(3);
const L1: Uuid = Uuid::from_u128(101);
const L2: Uuid = Uuid::from_u128(102);

fn day(d: u8) -> Date {
    Date::from_calendar_date(2026, Month::October, d).unwrap()
}
/// A frozen date basis covering no line, so every line's date row fails closed.
fn empty_basis() -> DateBasis {
    let row = PolicyRow {
        policy_id: PLATFORM_DEFAULT_POLICY_ID,
        resource_tenant_id: None,
        service_activation_required: false,
        acceptance_due_required: false,
        revision: 1,
    };
    let policy = DatePolicySnapshot::effective(RUN, &[row]).unwrap();
    DateBasis::resolve(RUN, policy, day(22), std::iter::empty())
}
fn line(id: Uuid) -> BasketLine {
    BasketLine {
        line_id: id,
        catalog: LineCatalogRequest {
            line_id: id,
            plan_id: PLAN,
            revision_id: REV,
            items: vec![SelectedItem {
                item_id: ITEM,
                quantity: "1".into(),
                selected_dim_value: None,
            }],
        },
        currency: Some("USD".into()),
        contract_ref: None,
    }
}
/// A real assessment of `lines` with every port down (the shaping reads only its report and
/// pins), then `L1`'s pin composed so both pin views are exercised.
fn assessment(lines: &[Uuid]) -> Assessment {
    let down = || Unavailable::Missing;
    let mut a = assess(&AssessmentInput {
        occupancy_axes: None,
        unsupported_terms: std::collections::BTreeSet::default(),
        operation: Operation::Preview,
        resolve_date: day(22),
        lines: lines.iter().copied().map(line).collect(),
        catalog: BTreeMap::new(),
        identity: PortAnswer::Unavailable(down()),
        contract_id: None,
        contracts: PortAnswer::Unavailable(down()),
        overlap_keys: PortAnswer::Unavailable(down()),
        occupancy: PortAnswer::Unavailable(down()),
        inflight_same_tuple: std::collections::BTreeSet::new(),
        inflight_other_orders: BTreeMap::new(),
        billing_terms: PortAnswer::Unavailable(down()),
        acceptance_policy: PortAnswer::Unavailable(down()),
        assessment: PortAnswer::Unavailable(down()),
        evaluation: PortAnswer::Unavailable(down()),
        dates: empty_basis(),
    })
    .unwrap();
    a.pins.insert(
        L1,
        PinOutcome::Composed {
            bindings: Vec::new(),
            revision: RevisionRef {
                plan_id: PLAN,
                revision_id: REV,
                revision_no: 1,
            },
        },
    );
    a
}
/// Deliberately non-canonical-looking but valid producer text: a computed value would differ.
fn amount(numerator: &str, denominator: &str, display: &str) -> Amount {
    Amount {
        exact: ExactMoney::new(numerator, denominator, "USD", 2).unwrap(),
        display: display.into(),
    }
}
fn evaluation(usage_on_l2: bool) -> OrderEvaluation {
    let totals = |n: &str, d: &str, s: &str| Totals {
        gross: amount(n, d, s),
        net: amount(n, d, s),
        discount: Availability::Available(amount("1", "3", "0.33 (one third)")),
    };
    OrderEvaluation {
        assessment_id: RUN,
        estimate: true,
        currency: "USD".into(),
        currency_scale: 2,
        lines: vec![
            LineEvaluation {
                line_id: L1,
                totals: totals("7", "3", "2.33"),
                usage_flagged: false,
                committed_components: None,
                declaration: None,
                usage_declaration: None,
            },
            LineEvaluation {
                line_id: L2,
                totals: totals("1", "1", "1.00"),
                usage_flagged: usage_on_l2,
                committed_components: None,
                declaration: None,
                usage_declaration: None,
            },
        ],
        // Not the sum of the lines: the shaping must copy, never add.
        order_totals: totals("1000", "7", "142.857142... USD"),
        order_committed_components: None,
        order_tcv_carrier: None,
        order_declaration: None,
        order_usage_declaration: None,
        tcv: Some(Tcv {
            exact: ExactMoney::new("12001", "100", "USD", 2).unwrap(),
            display: "120.01".into(),
            posting_estimate: "120.01".into(),
            basis: None,
            evidence: None,
        }),
        promotion_status: Availability::Unavailable,
        request_digest: "digest".into(),
    }
}
fn tax() -> IndicativeTax {
    IndicativeTax {
        per_line: vec![
            LineAmount {
                line_id: L2,
                amount: amount("5", "9", "0.56*"),
            },
            LineAmount {
                line_id: L1,
                amount: amount("1", "1", "1.00"),
            },
        ],
        // Not the sum of the lines either.
        total: amount("99", "1", "99.00"),
        explicit_no_tax: false,
    }
}
fn facts(id: Uuid, authored: bool, sad: Option<Date>) -> PreviewLineFacts {
    PreviewLineFacts {
        line_id: id,
        term_and_cycle_authored: authored,
        service_activation: sad,
    }
}

#[test]
fn the_tcv_is_copied_verbatim_when_every_line_authored_term_and_cycle() {
    let a = assessment(&[L1, L2]);
    let evaluated = evaluation(true);
    let taxed = tax();
    let lines = [
        facts(L1, true, Some(day(22))),
        facts(L2, true, Some(day(22))),
    ];
    let answers = PreviewAnswers {
        evaluation: Some(&evaluated),
        tax: Some(&taxed),
    };
    let result = preview_result(RUN, &a, answers, &lines, day(22));
    let tcv = result.tcv.expect("tcv");
    assert_eq!(
        (tcv.exact.numerator.as_str(), tcv.exact.denominator.as_str()),
        ("12001", "100")
    );
    assert_eq!(tcv.display, "120.01");
    assert!(result.tcv_basis.is_none(), "never invent a missing basis");
    assert!(result.tcv_withheld.is_none());
    // The resolved total is the producer's order total, not a sum of the lines.
    let total = result.resolved_total.expect("total");
    assert_eq!(total.gross.exact.numerator, "1000");
    assert_eq!(total.gross.exact.denominator, "7");
    assert_eq!(total.net.display, "142.857142... USD");
    assert_eq!((total.currency.as_str(), total.currency_scale), ("USD", 2));
    match total.discount {
        DiscountView::Available(d) => assert_eq!(d.display, "0.33 (one third)"),
        other => panic!("discount {other:?}"),
    }
    assert!(
        total.exclusions.is_none(),
        "never invent missing exclusions"
    );
    assert!(total.usage_flagged, "a usage-flagged line flags the total");
    // The tax keeps the producer's per-line order and its stated total.
    let tax = result.indicative_tax.expect("tax");
    let per_line: Vec<(Uuid, &str)> = tax
        .per_line
        .iter()
        .map(|l| (l.line_id, l.tax.display.as_str()))
        .collect();
    assert_eq!(per_line, vec![(L2, "0.56*"), (L1, "1.00")]);
    assert_eq!(tax.per_line[0].tax.exact.numerator, "5");
    assert_eq!(tax.total.display, "99.00");
    assert!(!tax.explicit_no_tax);
    // Same dates as the proposed one: no deferral, no expected fulfillment time.
    assert!(result.lines.iter().all(|l| l.deferral.is_none()));
    assert!(result.expected_fulfillment_time.is_none());
}

#[test]
fn the_tcv_is_withheld_with_the_reason_and_every_lacking_line() {
    let a = assessment(&[L1, L2]);
    let e = evaluation(false);
    let lines = [
        facts(L1, false, Some(day(22))),
        facts(L2, false, Some(day(22))),
    ];
    let answers = PreviewAnswers {
        evaluation: Some(&e),
        tax: None,
    };
    let r = preview_result(RUN, &a, answers, &lines, day(22));
    assert!(
        r.tcv.is_none(),
        "withheld even though the evaluation has one"
    );
    let withheld = r.tcv_withheld.expect("withheld");
    assert_eq!(withheld.reason, "preview-term-or-cycle-missing");
    assert_eq!(withheld.reason, TCV_WITHHELD_REASON);
    assert_eq!(withheld.line_ids, vec![L1, L2]);
    // Not a refusal: every other member is still returned.
    assert!(r.resolved_total.is_some());
    assert!(!r.resolved_total.unwrap().usage_flagged);
    assert!(r.indicative_tax.is_none());
    assert_eq!(r.lines.len(), 2);
    // The wire name of the annotation is camelCase.
    let json = serde_json::to_value(preview_result(
        RUN,
        &a,
        PreviewAnswers::default(),
        &[facts(L1, true, None), facts(L2, false, None)],
        day(22),
    ))
    .unwrap();
    assert_eq!(
        json["tcvWithheld"],
        serde_json::json!({"reason": "preview-term-or-cycle-missing", "lineIds": [L2]})
    );
    assert!(json.get("tcv").is_none());
}

#[test]
fn a_run_without_an_evaluation_has_no_tcv_and_no_withheld_marker() {
    // Every line authored term and cycle and none is tcv-only: absence of the TCV is then only
    // the missing evaluation, which is not an annotation of its own.
    let a = assessment(&[L1]);
    let r = preview_result(
        RUN,
        &a,
        PreviewAnswers::default(),
        &[facts(L1, true, Some(day(22)))],
        day(22),
    );
    assert!(r.tcv.is_none());
    assert!(r.tcv_basis.is_none());
    assert!(r.tcv_withheld.is_none());
    assert!(r.resolved_total.is_none());
    assert!(r.indicative_tax.is_none());
}

/// `a` with `line`'s pin-composition row marked `tcv-only`, as `assess` marks a composed pin whose
/// billing terms were unavailable or missing on a Preview.
fn with_tcv_only_pin(mut a: Assessment, line: Uuid) -> Assessment {
    let mut outcomes = a.vector.outcomes().to_vec();
    let row = outcomes
        .iter_mut()
        .find(|o| o.key.predicate == PredicateId::PinComposition && o.key.line_id == Some(line))
        .expect("pin row");
    row.applicability = Applicability::TcvOnly;
    a.vector = Vector::build(outcomes).unwrap();
    a
}

#[test]
fn a_line_whose_terms_were_unavailable_withholds_the_tcv_though_it_authored_them() {
    let a = with_tcv_only_pin(assessment(&[L1, L2]), L2);
    let e = evaluation(false);
    // Both lines authored term and cycle; only L2's pin row is tcv-only (terms unavailable).
    let lines = [
        facts(L1, true, Some(day(22))),
        facts(L2, true, Some(day(22))),
    ];
    let answers = PreviewAnswers {
        evaluation: Some(&e),
        tax: None,
    };
    let r = preview_result(RUN, &a, answers, &lines, day(22));
    assert!(r.tcv.is_none(), "withheld though the evaluation has one");
    let withheld = r.tcv_withheld.expect("withheld");
    assert_eq!(withheld.reason, TCV_WITHHELD_REASON);
    assert_eq!(withheld.line_ids, vec![L2]);
    // A successful annotation: every other member is still returned.
    assert!(r.resolved_total.is_some());
    assert_eq!(r.lines.len(), 2);
    // A line lacking authored terms and one tcv-only are both listed, in basket order, once.
    let both = [
        facts(L1, false, Some(day(22))),
        facts(L2, true, Some(day(22))),
    ];
    let r = preview_result(RUN, &a, answers, &both, day(22));
    assert_eq!(r.tcv_withheld.expect("withheld").line_ids, vec![L1, L2]);
}

#[test]
fn deferral_and_expected_fulfillment_follow_the_service_activation_dates() {
    let a = assessment(&[L1, L2]);
    let lines = [
        facts(L1, true, Some(day(22))),
        facts(L2, true, Some(day(29))),
    ];
    let r = preview_result(RUN, &a, PreviewAnswers::default(), &lines, day(22));
    assert_eq!(r.lines[0].line_id, L1);
    assert_eq!(r.lines[0].deferral, None);
    assert_eq!(r.lines[1].line_id, L2);
    assert_eq!(r.lines[1].deferral, Some(CalendarDate(day(29))));
    assert_eq!(
        r.expected_fulfillment_time,
        Some(day(29).midnight().assume_utc())
    );
    // An unresolved cascade defers nothing; one later line alone sets the time.
    let lines = [facts(L1, true, None), facts(L2, true, Some(day(30)))];
    let r = preview_result(RUN, &a, PreviewAnswers::default(), &lines, day(22));
    assert_eq!(r.lines[0].deferral, None);
    assert_eq!(
        r.expected_fulfillment_time,
        Some(day(30).midnight().assume_utc())
    );
}

#[test]
fn the_report_and_pins_are_the_assessments() {
    let a = assessment(&[L1, L2]);
    let lines = [facts(L1, true, None), facts(L2, true, None)];
    let r = preview_result(RUN, &a, PreviewAnswers::default(), &lines, day(22));
    assert_eq!(r.assessment_id, RUN);
    assert_eq!(r.assessment, a.report(RUN));
    assert_eq!(r.lines[0].pin, PinOutcomeView::Composed);
    let PinOutcome::Unevaluable(reason) = a.pins[&L2] else {
        panic!("pin {:?}", a.pins[&L2]);
    };
    assert_eq!(
        r.lines[1].pin,
        PinOutcomeView::Unevaluable {
            reason: reason.token().to_owned()
        }
    );
    // A line the assessment has no pin for is unevaluable, never composed.
    let r = preview_result(
        RUN,
        &a,
        PreviewAnswers::default(),
        &[facts(Uuid::from_u128(999), true, None)],
        day(22),
    );
    assert_eq!(
        r.lines[0].pin,
        PinOutcomeView::Unevaluable {
            reason: Reason::ReferenceUnresolvable.token().to_owned()
        }
    );
}

/// Preview never reaches submit's phase B: the Preview service, its shaping and the
/// orchestrator (both halves) name no sellability port and no `check` call.
#[test]
fn preview_never_names_the_phase_b_check() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for file in [
        "src/infra/gate/preview.rs",
        "src/domain/gate/report.rs",
        "src/infra/gate/orchestrate.rs",
        "src/domain/gate/orchestrate.rs",
    ] {
        let text = std::fs::read_to_string(root.join(file)).unwrap();
        for forbidden in ["SellabilityV1", "check("] {
            assert!(!text.contains(forbidden), "{file} names `{forbidden}`");
        }
    }
}

#[test]
fn the_per_caller_limits_default_and_are_bounded() {
    let config = GateConfig::default();
    assert_eq!(config.preview_per_caller_per_minute, 60);
    assert_eq!(config.submit_per_caller_per_minute, 10);
    assert_eq!(config.preview_limit().unwrap().per_minute.get(), 60);
    assert_eq!(config.submit_limit().unwrap().per_minute.get(), 10);
    let parsed: GateConfig = serde_json::from_value(serde_json::json!({})).unwrap();
    assert_eq!(parsed.preview_per_caller_per_minute, 60);
    assert!(parsed.validate().is_ok());
    for (key, value, ok) in [
        ("preview_per_caller_per_minute", 0, false),
        ("preview_per_caller_per_minute", 1, true),
        ("preview_per_caller_per_minute", 600, true),
        ("preview_per_caller_per_minute", 601, false),
        ("submit_per_caller_per_minute", 0, false),
        ("submit_per_caller_per_minute", 601, false),
    ] {
        let config: GateConfig = serde_json::from_value(serde_json::json!({ key: value })).unwrap();
        assert_eq!(config.validate().is_ok(), ok, "{key}={value}");
    }
}

/// The served body is the SDK serialization byte for byte (one source for the bytes and the
/// `OpenAPI` schema), every optional member included.
#[test]
fn the_rest_body_is_the_sdk_serialization() {
    use bss_orders::api::rest::dto::OrdersPreviewResult;
    let a = assessment(&[L1, L2]);
    let evaluated = evaluation(true);
    let taxed = tax();
    for (authored, sad) in [(true, day(22)), (false, day(29))] {
        let result = preview_result(
            RUN,
            &a,
            PreviewAnswers {
                evaluation: Some(&evaluated),
                tax: Some(&taxed),
            },
            &[
                facts(L1, true, Some(day(22))),
                facts(L2, authored, Some(sad)),
            ],
            day(22),
        );
        let sdk = serde_json::to_value(&result).unwrap();
        let rest = serde_json::to_value(OrdersPreviewResult::try_from(result).unwrap()).unwrap();
        assert_eq!(rest, sdk);
    }
}

#[test]
fn the_rest_body_decodes_through_the_sdk_rules() {
    use bss_orders::api::rest::gate::preview_body;
    let body = serde_json::json!({
        "resource_tenant_id": L1,
        "payer_tenant_id": L1,
        "seller_tenant_id": L2,
        "lines": [{
            "plan_id": PLAN,
            "plan_revision_id": REV,
            "selected_items": [{"item_id": ITEM, "quantity": "2.5"}],
            "currency": "USD",
            "service_activation_date": "2026-10-29",
            "billing_cycle": "year",
        }],
    });
    let basket = preview_body(body.to_string().as_bytes()).unwrap();
    assert_eq!(basket.lines.len(), 1);
    assert!(basket.lines[0].term_duration.is_none());
    for bad in [
        serde_json::json!({"unknown": 1}),
        {
            let mut b = body.clone();
            b["lines"][0]["expected_draft_revision"] = serde_json::json!(1);
            b
        },
        {
            let mut b = body;
            b["lines"][0]["service_activation_date"] = serde_json::json!("29/10/2026");
            b
        },
    ] {
        let refused = preview_body(bad.to_string().as_bytes()).unwrap_err();
        assert_eq!(refused.reason(), Some(Reason::RequestInvalid), "{bad}");
    }
}

#[test]
fn preview_preserves_declared_exclusions_and_every_tcv_basis() {
    use bss_orders::domain::gate::answers::{EvaluationChargeKind, TcvBasis, TotalsDeclaration};
    for (basis, token) in [
        (TcvBasis::FiniteTerm, "finite_term"),
        (TcvBasis::RollingAnnualized, "rolling_annualized"),
        (TcvBasis::Mixed, "mixed"),
    ] {
        let mut e = evaluation(false);
        e.tcv.as_mut().unwrap().basis = Some(basis);
        e.order_declaration = Some(TotalsDeclaration {
            charge_kind: EvaluationChargeKind::Recurring,
            amount_basis: "producer-horizon".into(),
            period_evidence: serde_json::json!({}),
            item_breakdown: serde_json::json!([]),
            recurring_by_cycle: serde_json::json!({}),
            exclusions: serde_json::json!({"tax": true, "brand": ["later"]}),
        });
        for authored in [true, false] {
            let r = preview_result(
                RUN,
                &assessment(&[L1]),
                PreviewAnswers {
                    evaluation: Some(&e),
                    tax: None,
                },
                &[facts(L1, authored, None)],
                day(22),
            );
            let dto = bss_orders::api::rest::dto::OrdersPreviewResult::try_from(r.clone()).unwrap();
            let wire = serde_json::to_value(r).unwrap();
            assert_eq!(serde_json::to_value(dto).unwrap(), wire);
            assert_eq!(
                wire["resolved_total"]["exclusions"],
                serde_json::json!({"tax": true, "brand": ["later"]})
            );
            if authored {
                assert_eq!(wire["tcv_basis"], token);
            } else {
                assert!(wire.get("tcv_basis").is_none());
            }
        }
    }
}

#[test]
fn preview_keeps_producer_aggregate_when_explicit_components_are_present() {
    use bss_orders::domain::gate::answers::{
        CommittedComponent, EvaluationChargeKind, TotalsDeclaration,
    };
    let mut e = evaluation(false);
    let mut component_totals = e.order_totals.clone();
    component_totals.gross = amount("5", "1", "5.00");
    e.order_committed_components = Some(vec![CommittedComponent {
        totals: component_totals,
        declaration: TotalsDeclaration {
            charge_kind: EvaluationChargeKind::OneTime,
            amount_basis: "once".into(),
            period_evidence: serde_json::json!({}),
            item_breakdown: serde_json::json!([]),
            recurring_by_cycle: serde_json::json!({}),
            exclusions: serde_json::json!({"usage": "excluded"}),
        },
    }]);
    let result = preview_result(
        RUN,
        &assessment(&[L1, L2]),
        PreviewAnswers {
            evaluation: Some(&e),
            tax: Some(&tax()),
        },
        &[facts(L1, true, None), facts(L2, true, None)],
        day(22),
    );
    let aggregate = result.resolved_total.unwrap();
    assert_eq!(aggregate.gross.exact.numerator, "1000");
    assert_eq!(aggregate.gross.exact.denominator, "7");
    assert_eq!(aggregate.gross.display, "142.857142... USD");
    assert!(
        aggregate.exclusions.is_none(),
        "component exclusions cannot invent an aggregate declaration"
    );
    assert_eq!(result.indicative_tax.unwrap().total.display, "99.00");
}
