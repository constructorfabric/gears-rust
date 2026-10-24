#![allow(clippy::expect_used, clippy::unwrap_used)]

use rust_decimal::Decimal;
use time::macros::{date, datetime};

use super::*;
use crate::input::{
    AdjustmentKind, AdjustmentRef, CoreTier, FactKind, Granule, InputQuantity, SuspensionPosture,
};
use crate::outcome::MoneyText;
use crate::test_input::{
    PERIOD_END, PERIOD_START, binding, cell, eur, hand_over, id, item, only_binding, period_line,
    price_id, slice,
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
        (total.covered_seconds, total.period_seconds),
        (2_592_000, 2_592_000)
    );
}

#[test]
fn v38_three_prorated_thirds_sum_to_one_on_the_price_total() {
    let mut input = period_line(flat("1.00"), "1");
    input.quantities.slices = vec![
        slice(PERIOD_START, DAY_11, "1"),
        slice(DAY_11, DAY_21, "1"),
        slice(DAY_21, PERIOD_END, "1"),
    ];
    let outcome = evaluate(&input).unwrap();
    for line in &outcome.lines {
        assert_eq!(fraction(&line.exact_amount), ("1", "3"));
        assert_eq!(line.lineage.proration.unwrap().covered_seconds, 864_000);
    }
    assert_eq!(fraction(&outcome.price_totals[0].rated), ("1", "1"));
    assert_eq!(outcome.price_totals[0].covered_seconds, 2_592_000);
}

#[test]
fn the_same_input_gives_the_same_bytes() {
    let mut input = period_line(per_unit("0.015"), "12");
    input.quantities.slices = vec![
        slice(PERIOD_START, DAY_11, "12"),
        slice(DAY_11, PERIOD_END, "12"),
    ];
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
    only_binding(&mut input).price.model = crate::input::CorePriceModel::Graduated {
        tiers: vec![CoreTier {
            up_to: None,
            rate: Decimal::ONE,
        }],
    };
    assert_eq!(reason(&input), "unsupported_primitive");
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
fn a_usage_window_waits_for_the_derived_meter() {
    let mut input = period_line(per_unit("0.047"), "10");
    input.child.kind = ChildKind::UsageWindow;
    input.fact.kind = FactKind::Usage;
    only_binding(&mut input).kind = ChargeKind::Usage;
    assert_eq!(reason(&input), "unsupported_primitive");
    input.derived = Some(crate::test_input::cloudlet());
    let error = evaluate(&input).unwrap_err();
    assert_eq!(error.reason_code(), "unsupported_primitive");
    assert!(error.detail().contains("derived meter"), "{error}");
    input
        .derived
        .as_mut()
        .unwrap()
        .declaration
        .output_unit
        .clear();
    assert_eq!(reason(&input), "derived_evaluation_failed");
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
fn suspension_proration_is_not_in_this_build() {
    let mut input = period_line(flat("30"), "1");
    input.fact.suspension_posture = SuspensionPosture::PauseRecurring;
    input.fact.suspensions.push(Interval {
        start: DAY_11,
        end: DAY_21,
    });
    assert_eq!(reason(&input), "unsupported_primitive");
    input.fact.suspension_posture = SuspensionPosture::Continue;
    evaluate(&input).unwrap();
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
    input.quantities.slices = vec![slice(
        PERIOD_START,
        datetime!(2026-09-04 00:00:00 UTC),
        &quantity,
    )];
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
