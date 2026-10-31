#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Every input this module refuses, by the reason the design gives (the review probes r1–r14 of
//! PR #5452, now expecting the right error).

use rust_decimal::Decimal;
use time::macros::{date, datetime};

use crate::evaluate;
use crate::exact::ExactQuantity;
use crate::input::{
    ChildKind, CorePriceModel, EvaluationInput, FactKind, Interval, PricePin, PriceState,
    QuantityInterval, RecurringPeriod,
};
use crate::test_input::{
    PERIOD_END, PERIOD_START, binding, cell, cloudlet, eur, hand_over, id, item, only_binding,
    period_line, price_id, slice,
};

const DAY_11: time::OffsetDateTime = datetime!(2026-09-11 00:00:00 UTC);

fn flat(amount: &str) -> CorePriceModel {
    CorePriceModel::Flat {
        amount: amount.parse().unwrap(),
    }
}

fn reason(input: &EvaluationInput) -> &'static str {
    evaluate(input).unwrap_err().reason_code()
}

#[test]
fn r1_a_period_start_price_the_fact_did_not_accept_is_a_binding_mismatch() {
    let mut input = period_line(flat("30"), "1");
    only_binding(&mut input).price.price_id = id(0x2_0999);
    assert_eq!(reason(&input), "binding_mismatch");
}

#[test]
fn an_item_the_accepted_binding_does_not_name_is_a_binding_mismatch() {
    let mut input = period_line(flat("30"), "1");
    input.fact.accepted.clear();
    assert_eq!(reason(&input), "binding_mismatch");
}

#[test]
fn r9_two_accepted_prices_for_one_value_are_a_binding_mismatch() {
    let mut input = period_line(flat("30"), "1");
    input.fact.accepted.push(PricePin {
        item_id: item(),
        dimension_value: None,
        price_id: id(0x2_0777),
    });
    assert_eq!(reason(&input), "binding_mismatch");
    let mut input = period_line(flat("30"), "1");
    let same = input.fact.accepted[0].clone();
    input.fact.accepted.push(same);
    evaluate(&input).unwrap();
}

#[test]
fn r12_a_price_that_is_not_approved_is_uncovered() {
    let mut input = period_line(flat("30"), "1");
    only_binding(&mut input).price.state = PriceState::Cancelled;
    assert_eq!(reason(&input), "price_uncovered");
}

#[test]
fn r8_the_first_resolve_is_the_period_start() {
    let mut input = period_line(flat("30"), "1");
    input.pricing.resolves[0].resolve_date = date!(2026 - 08 - 15);
    assert_eq!(reason(&input), "layout_mismatch");
}

#[test]
fn a_later_resolve_is_only_at_the_ends_on_of_the_binding_before() {
    let mut input = period_line(flat("30"), "1");
    hand_over(
        &mut input,
        date!(2026 - 09 - 11),
        binding(item(), id(0x2_0002), flat("60"), eur()),
    );
    input.quantities.slices = vec![
        slice(PERIOD_START, DAY_11, "1"),
        slice(DAY_11, PERIOD_END, "1"),
    ];
    evaluate(&input).unwrap();
    only_binding(&mut input).price.ends_on = None;
    assert_eq!(reason(&input), "layout_mismatch");
    only_binding(&mut input).price.ends_on = Some(date!(2026 - 09 - 12));
    assert_eq!(reason(&input), "layout_mismatch");
}

#[test]
fn r4_a_slice_across_a_handover_waits_for_split_points() {
    let mut input = period_line(flat("30"), "1");
    hand_over(
        &mut input,
        date!(2026 - 09 - 11),
        binding(item(), id(0x2_0002), flat("60"), eur()),
    );
    assert_eq!(reason(&input), "layout_mismatch");
}

#[test]
fn r2_a_window_past_the_billing_period_is_a_geometry_mismatch() {
    let mut input = period_line(flat("30"), "1");
    let november = datetime!(2026-11-01 00:00:00 UTC);
    input.child.window.window_end = november;
    input.child.window.served_to = november;
    input.quantities.slices = vec![slice(PERIOD_START, november, "1")];
    assert_eq!(reason(&input), "period_geometry_mismatch");
}

#[test]
fn r3_a_window_served_beyond_the_facts_served_extent_is_a_geometry_mismatch() {
    let mut input = period_line(flat("30"), "1");
    input.fact.served.end = DAY_11;
    assert_eq!(reason(&input), "period_geometry_mismatch");
    input.child.window.served_to = DAY_11;
    input.quantities.slices = vec![slice(PERIOD_START, DAY_11, "1")];
    evaluate(&input).unwrap();
}

#[test]
fn a_recurring_window_is_its_whole_billing_period() {
    let mut input = period_line(flat("30"), "1");
    input.child.window.window_start = DAY_11;
    input.child.window.served_from = DAY_11;
    input.quantities.slices = vec![slice(DAY_11, PERIOD_END, "1")];
    assert_eq!(reason(&input), "period_geometry_mismatch");
}

#[test]
fn r11_the_billing_period_matches_the_recurring_period() {
    let mut input = period_line(flat("1200"), "1");
    only_binding(&mut input).recurring_period = Some(RecurringPeriod::Year);
    assert_eq!(reason(&input), "period_geometry_mismatch");
    // A recurring price without a period is a malformed price, not a wrong billing period.
    only_binding(&mut input).recurring_period = None;
    let error = evaluate(&input).unwrap_err();
    assert_eq!(error.reason_code(), "unsupported_primitive");
    assert!(error.detail().contains("no recurring period"), "{error}");

    let mut year = period_line(flat("1200"), "1");
    only_binding(&mut year).recurring_period = Some(RecurringPeriod::Year);
    let end = datetime!(2027-09-01 00:00:00 UTC);
    year.fact.period.end = end;
    year.fact.served.end = end;
    year.child.window.window_end = end;
    year.child.window.served_to = end;
    year.quantities.slices = vec![slice(PERIOD_START, end, "1")];
    evaluate(&year).unwrap();
}

#[test]
fn a_month_ends_in_the_next_calendar_month_at_the_same_time() {
    for (start, end, fits) in [
        (
            datetime!(2027-02-01 00:00:00 UTC),
            datetime!(2027-03-01 00:00:00 UTC),
            true,
        ),
        (
            datetime!(2027-01-31 00:00:00 UTC),
            datetime!(2027-02-28 00:00:00 UTC),
            true,
        ),
        (
            datetime!(2026-12-15 14:00:00 UTC),
            datetime!(2027-01-15 14:00:00 UTC),
            true,
        ),
        (
            datetime!(2026-09-01 00:00:00 UTC),
            datetime!(2026-09-29 00:00:00 UTC),
            false,
        ),
        (
            datetime!(2026-09-01 00:00:00 UTC),
            datetime!(2026-10-01 01:00:00 UTC),
            false,
        ),
    ] {
        let mut input = period_line(flat("30"), "1");
        input.fact.period = Interval { start, end };
        input.fact.served = Interval { start, end };
        input.child.window.window_start = start;
        input.child.window.served_from = start;
        input.child.window.window_end = end;
        input.child.window.served_to = end;
        input.pricing.resolves[0].resolve_date = start.date();
        input.quantities.slices = vec![slice(start, end, "1")];
        let result = evaluate(&input);
        assert_eq!(result.is_ok(), fits, "[{start}, {end}): {result:?}");
    }
}

#[test]
fn r5_a_recurring_slice_carries_the_committed_quantity() {
    let per_unit = CorePriceModel::PerUnit {
        unit_amount: Decimal::TEN,
    };
    let mut input = period_line(per_unit.clone(), "1");
    input.quantities.slices[0].q = ExactQuantity::parse("100").unwrap();
    assert_eq!(reason(&input), "layout_mismatch");
    let mut input = period_line(per_unit, "1");
    input.fact.quantity = None;
    assert_eq!(reason(&input), "layout_mismatch");
    let mut input = period_line(flat("30"), "1");
    input.fact.quantity = None;
    evaluate(&input).unwrap();
}

#[test]
fn committed_quantity_changes_are_split_points_of_the_recurring_line() {
    let per_unit = CorePriceModel::PerUnit {
        unit_amount: Decimal::TEN,
    };
    let interval = |start, end, q: &str| QuantityInterval {
        interval: Interval { start, end },
        quantity: ExactQuantity::parse(q).unwrap(),
    };
    let mut input = period_line(per_unit, "1");
    input.subscription.quantity_intervals = vec![
        interval(PERIOD_START, DAY_11, "1"),
        interval(DAY_11, PERIOD_END, "2"),
    ];
    // Not cut at the change: the slices are not the layout of the split points.
    assert_eq!(reason(&input), "layout_mismatch");
    input.quantities.slices = vec![
        slice(PERIOD_START, DAY_11, "1"),
        slice(DAY_11, PERIOD_END, "1"),
    ];
    // Cut, but the second slice does not carry the quantity in force over it.
    assert_eq!(reason(&input), "layout_mismatch");
    input.quantities.slices[1] = slice(DAY_11, PERIOD_END, "2");
    evaluate(&input).unwrap();
    // A slice no interval holds has no committed quantity.
    input.subscription.quantity_intervals[1].interval.end = datetime!(2026-09-30 00:00:00 UTC);
    input.quantities.slices = vec![
        slice(PERIOD_START, DAY_11, "1"),
        slice(DAY_11, datetime!(2026-09-30 00:00:00 UTC), "2"),
        slice(datetime!(2026-09-30 00:00:00 UTC), PERIOD_END, "2"),
    ];
    assert_eq!(reason(&input), "layout_mismatch");
    // Overlapping intervals are refused before any slicing.
    input.subscription.quantity_intervals[1].interval.start = datetime!(2026-09-10 00:00:00 UTC);
    assert_eq!(reason(&input), "period_geometry_mismatch");
}

#[test]
fn r6_a_resolve_answers_one_cell_once() {
    let mut input = period_line(flat("30"), "1");
    let twin = binding(item(), price_id(), flat("45"), eur());
    input.pricing.resolves[0].cells.push(cell(twin));
    assert_eq!(reason(&input), "non_injective_mapping");
}

#[test]
fn r7_a_currency_bss_money_refuses_is_refused() {
    let mut input = period_line(flat("30"), "1");
    only_binding(&mut input).price.currency.scale = 200;
    assert_eq!(reason(&input), "binding_currency_mismatch");
    let mut input = period_line(flat("30"), "1");
    only_binding(&mut input).price.currency.code = "eur ".to_owned();
    input.fact.currency = "eur ".to_owned();
    assert_eq!(reason(&input), "binding_currency_mismatch");
}

#[test]
fn r13_a_decimal_without_canonical_text_is_refused() {
    let input = period_line(
        CorePriceModel::Flat {
            amount: Decimal::MAX,
        },
        "1",
    );
    assert!(serde_json::to_string(&input).is_err());
    assert_eq!(reason(&input), "precision_overflow");
    let mut input = period_line(flat("30"), "1");
    only_binding(&mut input).price.minimum_fee = Some(Decimal::MAX);
    assert_eq!(reason(&input), "precision_overflow");
}

#[test]
fn r14_a_derived_declaration_belongs_to_a_usage_child_only() {
    let mut input = period_line(flat("30"), "1");
    input.derived = Some(cloudlet());
    assert_eq!(reason(&input), "unsupported_primitive");
}

#[test]
fn one_time_children_wait_for_r19() {
    let mut input = period_line(flat("30"), "1");
    input.child.kind = ChildKind::OneTime;
    input.fact.kind = FactKind::OneTime;
    assert_eq!(reason(&input), "unsupported_primitive");
}

#[test]
fn a_binding_that_ends_inside_the_window_needs_its_successor() {
    let mut input = period_line(flat("30"), "1");
    only_binding(&mut input).price.ends_on = Some(date!(2026 - 09 - 11));
    input.quantities.slices = vec![
        slice(PERIOD_START, DAY_11, "1"),
        slice(DAY_11, PERIOD_END, "1"),
    ];
    assert_eq!(reason(&input), "layout_mismatch");
    // An end after the served window needs no successor, and cuts nothing.
    only_binding(&mut input).price.ends_on = Some(date!(2026 - 10 - 01));
    assert_eq!(reason(&input), "layout_mismatch");
    input.quantities.slices = vec![slice(PERIOD_START, PERIOD_END, "1")];
    evaluate(&input).unwrap();
    // Nor does an end the last resolve's successor already takes over from.
    let mut input = period_line(flat("30"), "1");
    hand_over(
        &mut input,
        date!(2026 - 09 - 11),
        binding(item(), id(0x2_0002), flat("60"), eur()),
    );
    input.quantities.slices = vec![
        slice(PERIOD_START, DAY_11, "1"),
        slice(DAY_11, PERIOD_END, "1"),
    ];
    input.pricing.resolves[1].cells[0]
        .binding
        .as_mut()
        .unwrap()
        .price
        .ends_on = Some(date!(2026 - 09 - 21));
    assert_eq!(reason(&input), "layout_mismatch");
}

#[test]
fn recurring_slices_with_a_gap_are_not_the_layout() {
    let mut input = period_line(flat("30"), "1");
    input.quantities.slices = vec![
        slice(PERIOD_START, DAY_11, "1"),
        slice(datetime!(2026-09-21 00:00:00 UTC), PERIOD_END, "1"),
    ];
    assert_eq!(reason(&input), "layout_mismatch");
}

#[test]
fn a_whole_second_with_another_offset_is_not_utc() {
    let mut input = period_line(flat("30"), "1");
    input.quantities.slices[0].slice_end = datetime!(2026-10-01 02:00:00 +02:00);
    assert_eq!(reason(&input), "period_geometry_mismatch");
}

#[test]
fn a_billing_currency_bss_money_refuses_is_refused() {
    let mut input = period_line(flat("30"), "1");
    input.fact.currency = "eur".to_owned();
    assert_eq!(reason(&input), "binding_currency_mismatch");
}

#[test]
fn an_input_without_resolves_has_no_resolve_in_force() {
    let mut input = period_line(flat("30"), "1");
    input.pricing.resolves.clear();
    assert!(super::pricing_resolves(&input).is_empty());
    assert_eq!(reason(&input), "price_uncovered");
}
