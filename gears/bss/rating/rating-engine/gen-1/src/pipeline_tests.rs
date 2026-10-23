#![allow(clippy::expect_used, clippy::unwrap_used)]

use rust_decimal::Decimal;
use time::OffsetDateTime;
use time::macros::{date, datetime};

use super::{CorePriceModel, EvaluationInput, Interval, PricePin, SliceQuantity, Step, evaluate};
use crate::exact::ExactQuantity;
use crate::generation::{ENGINE_DIGEST, GENERATION};
use crate::input::{
    AdjustmentKind, AdjustmentRef, CoreTier, DerivedExpr, FactKind, Granule, GranuleFold,
    GroupContext, GroupMember, InputQuantity, MemberSlice, RatingWindowKind, SuspensionPosture,
};
use crate::outcome::{ModelKind, MoneyText};
use crate::test_input::{
    PERIOD_END, PERIOD_START, binding, cell, commit, eur, granule, hand_over, id, item,
    only_binding, period_line, price_id, serve, slice, usage_binding, usage_window,
};

fn decimal(text: &str) -> Decimal {
    text.parse().unwrap()
}

fn flat(amount: &str) -> crate::input::CorePriceModel {
    crate::input::CorePriceModel::Flat {
        amount: decimal(amount),
    }
}

fn per_unit(rate: &str) -> crate::input::CorePriceModel {
    crate::input::CorePriceModel::PerUnit {
        unit_amount: decimal(rate),
    }
}

fn fraction(money: &MoneyText) -> (&str, &str) {
    (money.numerator.as_str(), money.denominator.as_str())
}

fn reason(input: &EvaluationInput) -> &'static str {
    evaluate(input).unwrap_err().reason_code()
}

/// Bands `[0, 10)` at 0.02 and `[10, ∞)` at 0.015 (Atlas F23, F24; pricing's cloudlet fixtures).
fn tiers() -> Vec<CoreTier> {
    vec![
        CoreTier {
            up_to: Some(decimal("10")),
            rate: decimal("0.02"),
        },
        CoreTier {
            up_to: None,
            rate: decimal("0.015"),
        },
    ]
}

fn volume() -> crate::input::CorePriceModel {
    crate::input::CorePriceModel::Volume { tiers: tiers() }
}

fn per_unit_usage() -> crate::input::CorePriceModel {
    per_unit("1")
}

const DAY_11: OffsetDateTime = datetime!(2026-09-11 00:00:00 UTC);
const DAY_21: OffsetDateTime = datetime!(2026-09-21 00:00:00 UTC);

#[test]
fn a_whole_period_line_rates_its_price() {
    let outcome = evaluate(&period_line(per_unit("0.047"), "2.5")).unwrap();
    assert_eq!(outcome.lines.len(), 1);
    let line = &outcome.lines[0];
    assert_eq!(fraction(&line.exact_amount), ("47", "400"));
    assert_eq!((line.currency.as_str(), line.currency_scale), ("EUR", 2));
    assert_eq!(
        line.quantity.as_ref().map(ExactQuantity::as_str),
        Some("2.5")
    );
    assert_eq!(line.lineage.proration.unwrap().covered_seconds, 2_592_000);
    assert!(outcome.obligations.is_empty());
    let total = &outcome.price_totals[0];
    assert_eq!(total.price_id, price_id());
    assert_eq!(fraction(&total.rated), ("47", "400"));
    assert_eq!(
        (
            crate::geometry::total_seconds(&total.covered),
            total.period_seconds
        ),
        (2_592_000, 2_592_000)
    );
}

#[test]
fn v38_three_prorated_thirds_sum_to_one_on_the_price_total() {
    let mut input = period_line(flat("1.00"), "1");
    commit(
        &mut input,
        &[
            (PERIOD_START, DAY_11, "1"),
            (DAY_11, DAY_21, "1"),
            (DAY_21, PERIOD_END, "1"),
        ],
    );
    let outcome = evaluate(&input).unwrap();
    for line in &outcome.lines {
        assert_eq!(fraction(&line.exact_amount), ("1", "3"));
        assert_eq!(line.lineage.proration.unwrap().covered_seconds, 864_000);
    }
    assert_eq!(fraction(&outcome.price_totals[0].rated), ("1", "1"));
    assert_eq!(
        crate::geometry::total_seconds(&outcome.price_totals[0].covered),
        2_592_000
    );
}

#[test]
fn the_same_input_gives_the_same_bytes() {
    let mut input = period_line(per_unit("0.015"), "12");
    commit(
        &mut input,
        &[(PERIOD_START, DAY_11, "12"), (DAY_11, PERIOD_END, "12")],
    );
    let first = serde_json::to_vec(&evaluate(&input).unwrap()).unwrap();
    let copy: EvaluationInput =
        serde_json::from_slice(&serde_json::to_vec(&input).unwrap()).unwrap();
    let again = serde_json::to_vec(&evaluate(&copy).unwrap()).unwrap();
    assert_eq!(first, again);
}

#[test]
fn another_generation_digest_or_budget_is_engine_unavailable() {
    let mut input = period_line(flat("1"), "1");
    input.engine.engine_generation = 2;
    assert_eq!(reason(&input), "engine_unavailable");
    let mut input = period_line(flat("1"), "1");
    input.engine.engine_digest = "00".repeat(32);
    assert_eq!(reason(&input), "engine_unavailable");
    let mut input = period_line(flat("1"), "1");
    input.budget.reduced_digits = 300;
    assert_eq!(reason(&input), "engine_unavailable");
}

#[test]
fn a_line_carries_invoice_inputs_null_asc606_refs_and_its_snapshot() {
    let outcome = evaluate(&period_line(flat("30"), "1")).unwrap();
    let line = &outcome.lines[0];
    assert_eq!(
        (
            line.gl_code.as_str(),
            line.tax_category.as_str(),
            line.invoice_line_template.as_str()
        ),
        ("4000", "std", "Pro plan, {period}")
    );
    assert_eq!(line.performance_obligation_ref, None);
    assert_eq!(line.ssp_snapshot_pointer, None);
    assert_eq!(
        line.line_key,
        format!("{}|{}|=|recurring|4000/std|2", id(0xF), item())
    );
    let snapshot = &line.snapshot;
    assert_eq!(snapshot.engine_generation, GENERATION);
    assert_eq!(snapshot.engine_digest, ENGINE_DIGEST);
    assert_eq!(snapshot.resolve_date, date!(2026 - 09 - 01));
    assert_eq!(
        snapshot.accepted_binding.as_ref().map(|pin| pin.price_id),
        Some(price_id())
    );
    assert_eq!(snapshot.bindings[0].price_id, price_id());
    assert_eq!(line.lineage.money_digest, "aa".repeat(32));
    assert_eq!(
        line.lineage
            .steps
            .iter()
            .map(|record| record.step)
            .collect::<Vec<_>>(),
        vec![
            Step::Bind,
            Step::Meter,
            Step::Model,
            Step::Prorate,
            Step::NonNegative,
            Step::Precision
        ]
    );
}

#[test]
fn a_slice_after_an_ends_on_is_bound_by_the_later_resolve_and_not_compared() {
    let mut input = period_line(flat("30"), "1");
    let successor = id(0x2_0002);
    hand_over(
        &mut input,
        date!(2026 - 09 - 11),
        binding(item(), successor, flat("60"), eur()),
    );
    input.quantities.slices = vec![
        slice(PERIOD_START, DAY_11, "1"),
        slice(DAY_11, PERIOD_END, "1"),
    ];
    let outcome = evaluate(&input).unwrap();
    let (before, after) = (&outcome.lines[0], &outcome.lines[1]);
    assert_eq!(
        (before.price_id, fraction(&before.exact_amount)),
        (price_id(), ("10", "1"))
    );
    assert_eq!(
        (after.price_id, fraction(&after.exact_amount)),
        (successor, ("40", "1"))
    );
    assert!(before.snapshot.accepted_binding.is_some());
    assert_eq!(after.snapshot.accepted_binding, None);
    assert_eq!(before.line_key, after.line_key);
    assert_eq!(outcome.price_totals.len(), 2);
}

#[test]
fn an_uncovered_cell_is_never_free() {
    let mut input = period_line(flat("30"), "1");
    input.pricing.resolves[0].cells[0].binding = None;
    assert_eq!(reason(&input), "price_uncovered");
    let mut input = period_line(flat("30"), "1");
    input.pricing.resolves[0].cells[0].selection.item_id = id(0x1_0009);
    assert_eq!(reason(&input), "price_uncovered");
}

#[test]
fn more_than_one_accepted_value_per_item_is_unsupported() {
    let mut input = period_line(flat("30"), "1");
    input.fact.accepted.push(PricePin {
        item_id: item(),
        dimension_value: Some("eu".into()),
        price_id: price_id(),
    });
    assert_eq!(reason(&input), "unsupported_primitive");
}

#[test]
fn guards_run_after_the_model_in_the_fixed_order() {
    let mut input = period_line(flat("30"), "1");
    input.fact.adjustments.push(AdjustmentRef {
        kind: AdjustmentKind::Coupon,
        reference: "WELCOME10".into(),
    });
    assert_eq!(reason(&input), "adjustment_unsupported");
    // The bind step runs before the guards: an uncovered cell is reported, not the coupon.
    input.pricing.resolves[0].cells[0].binding = None;
    assert_eq!(reason(&input), "price_uncovered");
    // So does the meter: a formula Products would round is reported before the coupon.
    let hour = datetime!(2026-09-15 10:00:00 UTC);
    let mut usage = usage_window(
        per_unit("1"),
        RatingWindowKind::CalendarHourUtc,
        &[(hour, "1")],
    );
    usage.fact.adjustments.push(AdjustmentRef {
        kind: AdjustmentKind::Coupon,
        reference: "WELCOME10".into(),
    });
    assert_eq!(reason(&usage), "adjustment_unsupported");
    usage.derived.as_mut().unwrap().declaration.formula = DerivedExpr::DivConst {
        operand: Box::new(DerivedExpr::Input {
            name: "vm".to_owned(),
        }),
        divisor: decimal("3"),
    };
    assert_eq!(reason(&usage), "derived_formula_inexact");
    let mut input = period_line(flat("30"), "1");
    input.fact.currency = "USD".into();
    assert_eq!(reason(&input), "fx_not_supported");
}

#[test]
fn rating_val_02_fails_the_child_and_emits_nothing() {
    let mut input = period_line(flat("30"), "1");
    let mut twin = binding(id(0x1_0002), id(0x2_0003), flat("1"), eur());
    twin.sku_id = only_binding(&mut input).sku_id;
    input.pricing.resolves[0].cells.push(cell(twin));
    assert_eq!(reason(&input), "non_injective_mapping");
}

#[test]
fn v39_an_amount_beyond_the_numerator_bound_is_not_returned() {
    let rate = format!("1{}", "0".repeat(27));
    let largest = evaluate(&period_line(per_unit(&rate), &"9".repeat(57))).unwrap();
    assert_eq!(largest.lines[0].exact_amount.numerator.len(), 84);
    let beyond = period_line(per_unit(&rate), &format!("1{}", "0".repeat(57)));
    assert_eq!(reason(&beyond), "precision_overflow");
}

#[test]
fn f23_hourly_volume_windows_rate_alone_and_never_pool() {
    // Bands [0, 10) at 0.02 and [10, ∞) at 0.015: 8 then 12 cloudlet-hours rate 0.16 + 0.18 =
    // 0.34, never 20 × 0.015 = 0.30 (DESIGN §4.3 worked example A).
    let ten = datetime!(2026-09-15 10:00:00 UTC);
    let eleven = datetime!(2026-09-15 11:00:00 UTC);
    let mut amounts = Vec::new();
    for (hour, q) in [(ten, "8"), (eleven, "12")] {
        let input = usage_window(volume(), RatingWindowKind::CalendarHourUtc, &[(hour, q)]);
        let outcome = evaluate(&input).unwrap();
        let line = &outcome.lines[0];
        assert_eq!(line.model_kind, ModelKind::Volume);
        assert_eq!(line.band_offset, Some(ExactQuantity::zero()));
        assert_eq!(line.lineage.granules[0].output.as_str(), q);
        amounts.push(fraction(&line.exact_amount).0.to_owned());
        amounts.push(fraction(&line.exact_amount).1.to_owned());
    }
    assert_eq!(amounts, ["4", "25", "9", "50"]);
    let graduated = usage_window(
        CorePriceModel::Graduated { tiers: tiers() },
        RatingWindowKind::CalendarHourUtc,
        &[(eleven, "12")],
    );
    let line = &evaluate(&graduated).unwrap().lines[0];
    assert_eq!(fraction(&line.exact_amount), ("23", "100"));
}

#[test]
fn f29_a_partial_hour_keeps_whole_thresholds() {
    let ten = datetime!(2026-09-15 10:00:00 UTC);
    let half_past = datetime!(2026-09-15 10:30:00 UTC);
    let mut input = usage_window(volume(), RatingWindowKind::CalendarHourUtc, &[(ten, "4")]);
    input.fact.served.start = half_past;
    input.child.window.served_from = half_past;
    input.quantities.slices[0].slice_start = half_past;
    let line = &evaluate(&input).unwrap().lines[0];
    assert_eq!(fraction(&line.exact_amount), ("2", "25"));
    assert_eq!(line.lineage.proration, None);
}

#[test]
fn worked_example_b_monthly_graduated_and_volume() {
    // Bands [0, 100) at 1.00 and [100, ∞) at 0.80; usage 40 + 30 + 50 = 120.
    let tiers = vec![
        CoreTier {
            up_to: Some(decimal("100")),
            rate: decimal("1.00"),
        },
        CoreTier {
            up_to: None,
            rate: decimal("0.80"),
        },
    ];
    let hours = [
        (datetime!(2026-09-02 08:00:00 UTC), "40"),
        (datetime!(2026-09-10 08:00:00 UTC), "30"),
        (datetime!(2026-09-20 08:00:00 UTC), "50"),
    ];
    let graduated = usage_window(
        CorePriceModel::Graduated {
            tiers: tiers.clone(),
        },
        RatingWindowKind::BillingCycle,
        &hours,
    );
    let outcome = evaluate(&graduated).unwrap();
    assert_eq!(fraction(&outcome.lines[0].exact_amount), ("116", "1"));
    assert_eq!(
        outcome.lines[0].quantity,
        Some(ExactQuantity::parse("120").unwrap())
    );
    let volume = usage_window(
        CorePriceModel::Volume { tiers },
        RatingWindowKind::BillingCycle,
        &hours,
    );
    assert_eq!(
        fraction(&evaluate(&volume).unwrap().lines[0].exact_amount),
        ("96", "1")
    );
}

#[test]
fn v18_an_ends_on_inside_the_window_adds_a_slice_and_keeps_the_band_axis() {
    // DESIGN 03 §4.3: a temporary price ([0, 100) at 1.00, then 0.80) ends on 09-15; the price in
    // force after it is [0, 100) at 0.90, then 0.70. Slice A has 80 units, slice B 50.
    let first = vec![
        CoreTier {
            up_to: Some(decimal("100")),
            rate: decimal("1.00"),
        },
        CoreTier {
            up_to: None,
            rate: decimal("0.80"),
        },
    ];
    let second = vec![
        CoreTier {
            up_to: Some(decimal("100")),
            rate: decimal("0.90"),
        },
        CoreTier {
            up_to: None,
            rate: decimal("0.70"),
        },
    ];
    let fifteenth = datetime!(2026-09-15 00:00:00 UTC);
    for (graduated, expected) in [
        (true, [("80", "1"), ("39", "1")]),
        (false, [("64", "1"), ("35", "1")]),
    ] {
        let model = |tiers: &Vec<CoreTier>| {
            if graduated {
                CorePriceModel::Graduated {
                    tiers: tiers.clone(),
                }
            } else {
                CorePriceModel::Volume {
                    tiers: tiers.clone(),
                }
            }
        };
        let mut input = usage_window(
            model(&first),
            RatingWindowKind::BillingCycle,
            &[(datetime!(2026-09-05 08:00:00 UTC), "80")],
        );
        hand_over(
            &mut input,
            date!(2026 - 09 - 15),
            usage_binding(id(0x2_0002), model(&second), RatingWindowKind::BillingCycle),
        );
        input.quantities.slices = vec![
            SliceQuantity {
                slice_start: PERIOD_START,
                slice_end: fifteenth,
                q: ExactQuantity::parse("80").unwrap(),
                q_version: 1,
                granules: vec![granule(datetime!(2026-09-05 08:00:00 UTC), "80")],
            },
            SliceQuantity {
                slice_start: fifteenth,
                slice_end: PERIOD_END,
                q: ExactQuantity::parse("50").unwrap(),
                q_version: 1,
                granules: vec![granule(datetime!(2026-09-20 08:00:00 UTC), "50")],
            },
        ];
        let outcome = evaluate(&input).unwrap();
        let amounts: Vec<(&str, &str)> = outcome
            .lines
            .iter()
            .map(|line| fraction(&line.exact_amount))
            .collect();
        assert_eq!(amounts, expected, "graduated: {graduated}");
        assert_eq!(
            outcome.lines[1].band_offset,
            Some(ExactQuantity::parse("80").unwrap())
        );
        assert_eq!(outcome.lines[1].snapshot.accepted_binding, None);
        assert_eq!(outcome.price_totals.len(), 2);
    }
}

#[test]
fn a_usage_window_meters_its_granules_and_fails_closed() {
    let hour = datetime!(2026-09-15 10:00:00 UTC);
    let usage = || usage_window(volume(), RatingWindowKind::CalendarHourUtc, &[(hour, "8")]);
    let mut input = usage();
    input.derived = None;
    assert_eq!(reason(&input), "unsupported_primitive");
    // An uncovered cell has no binding, so no declaration: the bind step fails it (DESIGN §3.1).
    input.pricing.resolves[0].cells[0].binding = None;
    assert_eq!(reason(&input), "price_uncovered");
    let mut input = usage();
    input
        .derived
        .as_mut()
        .unwrap()
        .declaration
        .output_unit
        .clear();
    assert_eq!(reason(&input), "derived_evaluation_failed");
    let mut input = usage();
    input.derived.as_mut().unwrap().declaration.inputs[0].granule_fold = GranuleFold::Peak;
    assert_eq!(reason(&input), "unsupported_input_fold");
    let mut input = usage();
    input.quantities.slices[0].q = ExactQuantity::parse("9").unwrap();
    assert_eq!(reason(&input), "layout_mismatch");
    let mut input = usage();
    input.derived.as_mut().unwrap().version = 2;
    assert_eq!(reason(&input), "derived_evaluation_failed");
    let mut input = usage();
    input.child.aggregation_key.meter = Some("products.derived/other@1".to_owned());
    assert_eq!(reason(&input), "layout_mismatch");
    let mut input = usage();
    input.child.window.window_end = hour + time::Duration::HOUR * 2;
    input.child.window.served_to = input.child.window.window_end;
    input.quantities.slices[0].slice_end = input.child.window.window_end;
    assert_eq!(reason(&input), "period_geometry_mismatch");
    let mut input = usage();
    input.pricing.resolves[0].cells[0]
        .binding
        .as_mut()
        .unwrap()
        .usage_policy = None;
    assert_eq!(reason(&input), "unsupported_primitive");
    let mut input = usage();
    input.quantities.slices[0].granules[0].granule_start = hour + time::Duration::MINUTE;
    assert_eq!(reason(&input), "layout_mismatch");
}

#[test]
fn v57_a_formula_products_would_round_is_never_rated() {
    let hour = datetime!(2026-09-15 10:00:00 UTC);
    let input_named = |name: &str| {
        Box::new(DerivedExpr::Input {
            name: name.to_owned(),
        })
    };
    let constant = |text: &str| {
        Box::new(DerivedExpr::Const {
            value: decimal(text),
        })
    };
    let cases = [
        (
            DerivedExpr::Add {
                left: input_named("vm"),
                right: constant("0.0000000001"),
            },
            "100000000000000000000",
            Err("derived_formula_inexact"),
        ),
        (
            DerivedExpr::DivConst {
                operand: input_named("vm"),
                divisor: decimal("3"),
            },
            "1",
            Err("derived_formula_inexact"),
        ),
        (
            DerivedExpr::Add {
                left: input_named("vm"),
                right: constant("2"),
            },
            "1",
            Ok("3"),
        ),
        (
            DerivedExpr::Mul {
                left: input_named("vm"),
                right: input_named("vm"),
            },
            "0.5",
            Ok("0.25"),
        ),
    ];
    for (formula, a, expected) in cases {
        let mut input = usage_window(
            per_unit_usage(),
            RatingWindowKind::CalendarHourUtc,
            &[(hour, a)],
        );
        input.derived.as_mut().unwrap().declaration.formula = formula;
        if let Ok(q) = expected {
            input.quantities.slices[0].q = ExactQuantity::parse(q).unwrap();
        }
        match (evaluate(&input), expected) {
            (Ok(outcome), Ok(q)) => {
                assert_eq!(outcome.lines[0].quantity.as_ref().unwrap().as_str(), q);
            }
            (Err(error), Err(code)) => assert_eq!(error.reason_code(), code, "{error}"),
            (result, expected) => panic!("{result:?}, expected {expected:?}"),
        }
    }
}

#[test]
fn a_window_group_places_the_member_after_the_earlier_members() {
    // A plan change on 09-11 inside the September window: the earlier member counted 80, this
    // member counts 50; graduated places it at 80, volume selects the band of 130.
    let tiers = vec![
        CoreTier {
            up_to: Some(decimal("100")),
            rate: decimal("1.00"),
        },
        CoreTier {
            up_to: None,
            rate: decimal("0.80"),
        },
    ];
    let hours = [(datetime!(2026-09-20 08:00:00 UTC), "50")];
    for (graduated, expected) in [(true, ("44", "1")), (false, ("40", "1"))] {
        let model = if graduated {
            CorePriceModel::Graduated {
                tiers: tiers.clone(),
            }
        } else {
            CorePriceModel::Volume {
                tiers: tiers.clone(),
            }
        };
        let mut input = usage_window(model, RatingWindowKind::BillingCycle, &hours);
        input.fact.served.start = DAY_11;
        input.child.window.served_from = DAY_11;
        input.quantities.slices[0].slice_start = DAY_11;
        let own = &input.quantities.slices[0];
        input.group_context = Some(GroupContext {
            window_group_id: id(0x9_0001),
            members: vec![
                GroupMember {
                    child_id: id(0xC1),
                    fact_id: id(0xF1),
                    fact_version: 1,
                    served_from: PERIOD_START,
                    layout_version: 1,
                    slices: vec![MemberSlice {
                        slice_start: PERIOD_START,
                        slice_end: DAY_11,
                        q: ExactQuantity::parse("80").unwrap(),
                        q_version: 3,
                    }],
                },
                GroupMember {
                    child_id: input.child.child_id,
                    fact_id: input.fact.fact_id,
                    fact_version: input.fact.fact_version,
                    served_from: DAY_11,
                    layout_version: input.quantities.layout_version,
                    slices: vec![MemberSlice {
                        slice_start: own.slice_start,
                        slice_end: own.slice_end,
                        q: own.q.clone(),
                        q_version: own.q_version,
                    }],
                },
            ],
            prefix_before_this_member: ExactQuantity::parse("80").unwrap(),
            group_total: ExactQuantity::parse("130").unwrap(),
            context_digest: "77".repeat(32),
        });
        let outcome = evaluate(&input).unwrap();
        assert_eq!(fraction(&outcome.lines[0].exact_amount), expected);
        let mut wrong = input.clone();
        wrong.group_context.as_mut().unwrap().group_total = ExactQuantity::parse("131").unwrap();
        assert_eq!(reason(&wrong), "layout_mismatch");
    }
}

#[test]
fn a_minimum_fee_on_an_hourly_window_fails_closed_until_r34() {
    let hour = datetime!(2026-09-15 10:00:00 UTC);
    let mut input = usage_window(volume(), RatingWindowKind::CalendarHourUtc, &[(hour, "8")]);
    input.pricing.resolves[0].cells[0]
        .binding
        .as_mut()
        .unwrap()
        .price
        .minimum_fee = Some(decimal("30"));
    assert_eq!(reason(&input), "unsupported_primitive");
    let mut input = usage_window(volume(), RatingWindowKind::BillingCycle, &[(hour, "8")]);
    input.pricing.resolves[0].cells[0]
        .binding
        .as_mut()
        .unwrap()
        .price
        .minimum_fee = Some(decimal("30"));
    let outcome = evaluate(&input).unwrap();
    assert_eq!(
        crate::geometry::total_seconds(&outcome.price_totals[0].covered),
        2_592_000
    );
}

#[test]
fn a_model_its_charge_kind_does_not_take_is_refused() {
    let input = period_line(CorePriceModel::Volume { tiers: tiers() }, "1");
    assert_eq!(reason(&input), "unsupported_primitive");
    let hour = datetime!(2026-09-15 10:00:00 UTC);
    let input = usage_window(flat("1"), RatingWindowKind::CalendarHourUtc, &[(hour, "8")]);
    assert_eq!(reason(&input), "unsupported_primitive");
}

#[test]
fn inconsistent_inputs_fail_by_name() {
    let mut input = period_line(flat("1"), "1");
    input.quantities.slices[0].q = ExactQuantity::parse("-1").unwrap();
    assert_eq!(reason(&input), "negative_window_quantity");

    let mut input = period_line(flat("1"), "1");
    input.quantities.slices[0].granules.push(Granule {
        granule_start: PERIOD_START,
        inputs: vec![InputQuantity {
            usage_type_ref: "gts.x".into(),
            q: ExactQuantity::parse("-0.5").unwrap(),
        }],
    });
    assert_eq!(reason(&input), "negative_window_quantity");

    let mut input = period_line(flat("1"), "1");
    input.quantities.slices = vec![slice(PERIOD_START, datetime!(2026-10-02 00:00:00 UTC), "1")];
    assert_eq!(reason(&input), "layout_mismatch");

    let mut input = period_line(flat("1"), "1");
    input.quantities.slices = vec![
        slice(DAY_11, PERIOD_END, "1"),
        slice(PERIOD_START, DAY_11, "1"),
    ];
    assert_eq!(reason(&input), "layout_mismatch");

    let mut input = period_line(flat("1"), "1");
    input.quantities.slices.clear();
    assert_eq!(reason(&input), "layout_mismatch");

    let mut input = period_line(flat("1"), "1");
    input.quantities.slices[0].slice_end = datetime!(2026-09-30 23:59:59.5 UTC);
    assert_eq!(reason(&input), "period_geometry_mismatch");

    let mut input = period_line(flat("1"), "1");
    input.fact.served.end = datetime!(2026-10-05 00:00:00 UTC);
    assert_eq!(reason(&input), "period_geometry_mismatch");

    let mut input = period_line(flat("1"), "1");
    input.fact.kind = FactKind::Usage;
    assert_eq!(reason(&input), "unsupported_primitive");
}

#[test]
fn the_bindings_of_one_child_share_one_currency() {
    let mut input = period_line(flat("30"), "1");
    let usd = crate::input::CoreCurrency {
        code: "USD".into(),
        scale: 2,
    };
    hand_over(
        &mut input,
        date!(2026 - 09 - 11),
        binding(item(), id(0x2_0002), flat("60"), usd),
    );
    input.quantities.slices = vec![
        slice(PERIOD_START, DAY_11, "1"),
        slice(DAY_11, PERIOD_END, "1"),
    ];
    assert_eq!(reason(&input), "binding_currency_mismatch");
}

#[test]
fn two_stored_scales_of_one_price_keep_their_own_totals() {
    let mut input = period_line(flat("30"), "1");
    let kwd_like = crate::input::CoreCurrency {
        code: "EUR".into(),
        scale: 3,
    };
    hand_over(
        &mut input,
        date!(2026 - 09 - 11),
        binding(item(), price_id(), flat("30"), kwd_like),
    );
    input.quantities.slices = vec![
        slice(PERIOD_START, DAY_11, "1"),
        slice(DAY_11, PERIOD_END, "1"),
    ];
    let outcome = evaluate(&input).unwrap();
    assert_ne!(outcome.lines[0].line_key, outcome.lines[1].line_key);
    let scales: Vec<_> = outcome
        .price_totals
        .iter()
        .map(|total| (total.price_id, total.currency_scale, fraction(&total.rated)))
        .collect();
    assert_eq!(
        scales,
        vec![(price_id(), 2, ("10", "1")), (price_id(), 3, ("20", "1"))]
    );
}

#[test]
fn a_paused_suspension_prorates_over_the_unsuspended_part() {
    let mut input = period_line(flat("30"), "1");
    input.fact.suspension_posture = SuspensionPosture::PauseRecurring;
    input.fact.suspensions.push(Interval {
        start: DAY_11,
        end: DAY_21,
    });
    let outcome = evaluate(&input).unwrap();
    assert_eq!(fraction(&outcome.lines[0].exact_amount), ("20", "1"));
    assert_eq!(
        crate::geometry::total_seconds(&outcome.price_totals[0].covered),
        1_728_000
    );
    input.fact.suspension_posture = SuspensionPosture::Continue;
    let outcome = evaluate(&input).unwrap();
    assert_eq!(fraction(&outcome.lines[0].exact_amount), ("30", "1"));
    input.fact.suspensions.push(Interval {
        start: DAY_11,
        end: PERIOD_END,
    });
    assert_eq!(reason(&input), "period_geometry_mismatch");
}

#[test]
fn v29_a_plan_change_prorates_both_children_and_their_fractions_sum_to_one() {
    // Plan A 30.00 a month until the change, plan B 60.00 after it (feature 02 §7, 09 §3.6).
    for (change, first, second) in [
        (DAY_11, ("10", "1"), ("40", "1")),
        (
            datetime!(2026-09-11 14:00:00 UTC),
            ("127", "12"),
            ("233", "6"),
        ),
    ] {
        let mut before = period_line(flat("30.00"), "1");
        serve(&mut before, PERIOD_START, change);
        let mut after = period_line(flat("60.00"), "1");
        after.fact.sub_line_key = "plan#2".to_owned();
        serve(&mut after, change, PERIOD_END);
        let before = evaluate(&before).unwrap();
        let after = evaluate(&after).unwrap();
        assert_eq!(fraction(&before.lines[0].exact_amount), first);
        assert_eq!(fraction(&after.lines[0].exact_amount), second);
        let covered = before.lines[0].lineage.proration.unwrap().covered_seconds
            + after.lines[0].lineage.proration.unwrap().covered_seconds;
        assert_eq!(covered, 2_592_000);
    }
}

#[test]
fn recurring_slices_of_the_largest_committed_quantity_are_never_summed() {
    // Two slices of 9 × 10^59 would sum beyond 60 integer digits; a recurring line has no band
    // axis, so each slice is priced alone.
    let q = format!("9{}", "0".repeat(59));
    let mut input = period_line(per_unit("0.01"), &q);
    commit(
        &mut input,
        &[(PERIOD_START, DAY_11, &q), (DAY_11, PERIOD_END, &q)],
    );
    let outcome = evaluate(&input).unwrap();
    assert_eq!(outcome.lines.len(), 2);
    assert_eq!(outcome.lines[0].band_offset, None);
}

#[test]
fn a_committed_quantity_change_cuts_the_recurring_line() {
    // 10.00 per unit: 1 unit for days 1–15, 3 units for days 16–30.
    let sixteenth = datetime!(2026-09-16 00:00:00 UTC);
    let mut input = period_line(per_unit("10"), "1");
    commit(
        &mut input,
        &[(PERIOD_START, sixteenth, "1"), (sixteenth, PERIOD_END, "3")],
    );
    let outcome = evaluate(&input).unwrap();
    let amounts: Vec<(&str, &str)> = outcome
        .lines
        .iter()
        .map(|line| fraction(&line.exact_amount))
        .collect();
    assert_eq!(amounts, [("5", "1"), ("15", "1")]);
    assert_eq!(fraction(&outcome.price_totals[0].rated), ("20", "1"));
}

#[test]
fn a_minimum_fee_reaches_the_price_total() {
    let mut input = period_line(flat("5"), "1");
    only_binding(&mut input).price.minimum_fee = Some(decimal("30"));
    let outcome = evaluate(&input).unwrap();
    let fee = outcome.price_totals[0].minimum_fee.as_ref().unwrap();
    assert_eq!(fraction(fee), ("30", "1"));
}

#[test]
fn delivery_bounds_apply_to_the_line_not_to_intermediate_lineage() {
    // 10^27 × 10^57 = 10^84 leaves the delivery bound, but it is an intermediate: three days of a
    // thirty-day period deliver 10^83, which is inside it (DESIGN §3.3).
    let rate = format!("1{}", "0".repeat(27));
    let quantity = format!("1{}", "0".repeat(57));
    let mut input = period_line(per_unit(&rate), &quantity);
    serve(&mut input, PERIOD_START, datetime!(2026-09-04 00:00:00 UTC));
    let outcome = evaluate(&input).unwrap();
    let line = &outcome.lines[0];
    assert_eq!(
        fraction(&line.exact_amount),
        (format!("1{}", "0".repeat(83)).as_str(), "1")
    );
    let model = line
        .lineage
        .steps
        .iter()
        .find(|record| record.step == Step::Model)
        .and_then(|record| record.post.as_ref())
        .unwrap();
    assert_eq!(model.numerator.len(), 85);
    assert_eq!(fraction(&outcome.price_totals[0].rated).0.len(), 84);
}

#[test]
fn a_clamped_amount_beyond_the_delivery_bound_still_clamps_to_zero() {
    let input = period_line(
        per_unit(&format!("-1{}", "0".repeat(27))),
        &format!("1{}", "0".repeat(57)),
    );
    let outcome = evaluate(&input).unwrap();
    let line = &outcome.lines[0];
    assert_eq!(fraction(&line.exact_amount), ("0", "1"));
    assert_eq!(
        line.lineage.clamped_from.as_ref().unwrap().numerator,
        format!("-1{}", "0".repeat(84))
    );
}

#[test]
fn a_usage_policy_that_changes_inside_the_window_fails_closed() {
    // The successor at 09-15 rates the September window under another rating window or fold.
    let fifteenth = datetime!(2026-09-15 00:00:00 UTC);
    let mut input = usage_window(
        volume(),
        RatingWindowKind::BillingCycle,
        &[(datetime!(2026-09-05 08:00:00 UTC), "8")],
    );
    hand_over(
        &mut input,
        date!(2026 - 09 - 15),
        usage_binding(id(0x2_0002), volume(), RatingWindowKind::CalendarHourUtc),
    );
    input.quantities.slices = vec![
        SliceQuantity {
            slice_start: PERIOD_START,
            slice_end: fifteenth,
            q: ExactQuantity::parse("8").unwrap(),
            q_version: 1,
            granules: vec![granule(datetime!(2026-09-05 08:00:00 UTC), "8")],
        },
        SliceQuantity {
            slice_start: fifteenth,
            slice_end: PERIOD_END,
            q: ExactQuantity::zero(),
            q_version: 1,
            granules: Vec::new(),
        },
    ];
    assert_eq!(reason(&input), "intra_window_policy_change");
    // The same policy content under another identity is no change (T-D-36).
    let successor = input.pricing.resolves[1].cells[0].binding.as_mut().unwrap();
    let policy = successor.usage_policy.as_mut().unwrap();
    policy.rating_window = RatingWindowKind::BillingCycle;
    policy.policy_id = id(0x6_0002);
    evaluate(&input).unwrap();
}

#[test]
fn a_usage_window_scoped_unlike_its_policy_is_a_layout_mismatch() {
    let hour = datetime!(2026-09-15 10:00:00 UTC);
    let mut input = usage_window(volume(), RatingWindowKind::CalendarHourUtc, &[(hour, "8")]);
    input.pricing.resolves[0].cells[0]
        .binding
        .as_mut()
        .unwrap()
        .usage_policy
        .as_mut()
        .unwrap()
        .aggregation_scope = crate::input::PolicyScope::Resource;
    assert_eq!(reason(&input), "layout_mismatch");
}

/// A named change that makes a window-group context stale.
type StaleGroup = (&'static str, fn(&mut GroupContext));

/// A window group around `input`'s one usage slice: an earlier member of 80, then the child.
fn group_of(input: &EvaluationInput) -> GroupContext {
    let own = &input.quantities.slices[0];
    GroupContext {
        window_group_id: id(0x9_0001),
        members: vec![
            GroupMember {
                child_id: id(0xC1),
                fact_id: id(0xF1),
                fact_version: 1,
                served_from: PERIOD_START,
                layout_version: 1,
                slices: vec![MemberSlice {
                    slice_start: PERIOD_START,
                    slice_end: DAY_11,
                    q: ExactQuantity::parse("80").unwrap(),
                    q_version: 3,
                }],
            },
            GroupMember {
                child_id: input.child.child_id,
                fact_id: input.fact.fact_id,
                fact_version: input.fact.fact_version,
                served_from: input.child.window.served_from,
                layout_version: input.quantities.layout_version,
                slices: vec![MemberSlice {
                    slice_start: own.slice_start,
                    slice_end: own.slice_end,
                    q: own.q.clone(),
                    q_version: own.q_version,
                }],
            },
        ],
        prefix_before_this_member: ExactQuantity::parse("80").unwrap(),
        group_total: ExactQuantity::parse("130").unwrap(),
        context_digest: "77".repeat(32),
    }
}

#[test]
fn a_stale_or_foreign_window_group_context_is_refused() {
    let mut input = usage_window(
        volume(),
        RatingWindowKind::BillingCycle,
        &[(datetime!(2026-09-20 08:00:00 UTC), "50")],
    );
    input.fact.served.start = DAY_11;
    input.child.window.served_from = DAY_11;
    input.quantities.slices[0].slice_start = DAY_11;
    input.group_context = Some(group_of(&input));
    evaluate(&input).unwrap();
    let variants: [StaleGroup; 6] = [
        ("members out of order", |group| group.members.reverse()),
        ("the child missing", |group| {
            group.members[1].child_id = id(0xC9);
        }),
        ("another fact version", |group| {
            group.members[1].fact_version = 2;
        }),
        ("another q_version", |group| {
            group.members[1].slices[0].q_version = 9;
        }),
        ("a wrong prefix", |group| {
            group.prefix_before_this_member = ExactQuantity::parse("79").unwrap();
        }),
        ("a wrong total", |group| {
            group.group_total = ExactQuantity::parse("131").unwrap();
        }),
    ];
    for (case, change) in variants {
        let mut stale = input.clone();
        change(stale.group_context.as_mut().unwrap());
        assert_eq!(reason(&stale), "layout_mismatch", "{case}");
    }
    // Only usage windows span facts.
    let mut recurring = period_line(flat("30"), "1");
    recurring.group_context = Some(group_of(&recurring));
    assert_eq!(reason(&recurring), "unsupported_primitive");
}

#[test]
fn a_negative_member_quantity_in_a_window_group_is_refused() {
    // The earlier member's 80 becomes -80 with a matching prefix and total, so the context stays
    // consistent; graduated would place this member's 50 at [-80, -30), below every band, and
    // rate it as nothing.
    let mut input = usage_window(
        CorePriceModel::Graduated { tiers: tiers() },
        RatingWindowKind::BillingCycle,
        &[(datetime!(2026-09-20 08:00:00 UTC), "50")],
    );
    input.fact.served.start = DAY_11;
    input.child.window.served_from = DAY_11;
    input.quantities.slices[0].slice_start = DAY_11;
    let mut group = group_of(&input);
    group.members[0].slices[0].q = ExactQuantity::parse("-80").unwrap();
    group.prefix_before_this_member = ExactQuantity::parse("-80").unwrap();
    group.group_total = ExactQuantity::parse("-30").unwrap();
    input.group_context = Some(group);
    assert_eq!(reason(&input), "negative_window_quantity");
}
