#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::num::NonZeroU32;

use rust_decimal::Decimal;
use uuid::Uuid;

use super::{
    AmountBasis, AmountStatus, ArithmeticBudget, ChargeKind, ChargeRow, CorePriceModel,
    DiscountStatus, OrderAmount, OrderEvaluationInput, OrderItem, OrderLine, OrderTerm, PriceState,
    RatingWindowKind, RecurringPeriod, evaluate_order,
};
use crate::input::CoreTier;
use crate::test_input::{binding, eur, id, item, period_line, price_id, quantity, usage_binding};

fn decimal(text: &str) -> Decimal {
    text.parse().unwrap()
}

fn per_unit(rate: &str) -> CorePriceModel {
    CorePriceModel::PerUnit {
        unit_amount: decimal(rate),
    }
}

fn flat(amount: &str) -> CorePriceModel {
    CorePriceModel::Flat {
        amount: decimal(amount),
    }
}

fn recurring(item_id: Uuid, model: CorePriceModel, q: &str) -> OrderItem {
    OrderItem {
        item_id,
        quantity: quantity(q),
        binding: binding(item_id, id(0x2_0000 + item_id.as_u128()), model, eur()),
    }
}

fn one_time(item_id: Uuid, model: CorePriceModel) -> OrderItem {
    let mut item = recurring(item_id, model, "1");
    item.binding.kind = ChargeKind::OneTime;
    item.binding.recurring_period = None;
    item
}

fn usage(item_id: Uuid) -> OrderItem {
    let tiers = vec![
        CoreTier {
            up_to: Some(decimal("10")),
            rate: decimal("0.02"),
        },
        CoreTier {
            up_to: None,
            rate: decimal("0.015"),
        },
    ];
    let mut binding = usage_binding(
        id(0x2_0000 + item_id.as_u128()),
        CorePriceModel::Volume { tiers },
        RatingWindowKind::CalendarHourUtc,
    );
    binding.item_id = item_id;
    OrderItem {
        item_id,
        quantity: quantity("1"),
        binding,
    }
}

fn line(n: u128, term: OrderTerm, cycle: RecurringPeriod, items: Vec<OrderItem>) -> OrderLine {
    OrderLine {
        line_id: id(0xA000 + n),
        plan_revision_id: id(0x7),
        term,
        cycle: Some(cycle),
        items,
    }
}

fn order(lines: Vec<OrderLine>) -> OrderEvaluationInput {
    OrderEvaluationInput {
        assessment_id: id(0xA55E),
        currency: "EUR".to_owned(),
        contract_ref: None,
        lines,
        budget: ArithmeticBudget::GEN_1,
        engine: crate::generation::engine_id(),
    }
}

fn periods(count: u32) -> NonZeroU32 {
    NonZeroU32::new(count).unwrap()
}

fn pair(amount: &OrderAmount) -> (&str, &str) {
    (amount.exact.as_str(), amount.display.as_str())
}

fn reason(input: &OrderEvaluationInput) -> &'static str {
    evaluate_order(input).unwrap_err().reason_code()
}

fn row(rows: &[ChargeRow], kind: ChargeKind) -> &ChargeRow {
    rows.iter().find(|row| row.charge_kind == kind).unwrap()
}

#[test]
fn v48_a_rolling_month_is_annualized_with_exact_and_display_amounts() {
    let input = order(vec![line(
        1,
        OrderTerm::Rolling,
        RecurringPeriod::Month,
        vec![recurring(item(), per_unit("0.333"), "1")],
    )]);
    let evaluation = evaluate_order(&input).unwrap();
    let item = &evaluation.lines[0].items[0];
    assert_eq!(pair(item.gross.as_ref().unwrap()), ("0.333", "0.33"));
    assert_eq!(item.discount_status, DiscountStatus::Unavailable);
    let tcv = evaluation.tcv.unwrap();
    assert_eq!(pair(&tcv.amount), ("3.996", "4"));
    assert_eq!(tcv.basis, AmountBasis::RollingAnnualized);
    assert_eq!(tcv.evidence[0].horizon.periods, 12);
    let recurring = row(&evaluation.order, ChargeKind::Recurring);
    assert_eq!(
        pair(&recurring.recurring_by_cycle[0].gross),
        ("0.333", "0.33")
    );
    assert_eq!(pair(recurring.gross.as_ref().unwrap()), ("3.996", "4"));
    assert_eq!(recurring.amount_basis, Some(AmountBasis::RollingAnnualized));
    assert_eq!(
        (evaluation.currency.as_str(), evaluation.currency_scale),
        ("EUR", 2)
    );
}

#[test]
fn a_finite_term_counts_its_periods_and_one_time_once() {
    let input = order(vec![line(
        1,
        OrderTerm::FixedPeriods { count: periods(6) },
        RecurringPeriod::Month,
        vec![
            recurring(item(), flat("10"), "3"),
            one_time(id(0x1_0002), flat("5")),
        ],
    )]);
    let evaluation = evaluate_order(&input).unwrap();
    // A flat price takes no quantity.
    assert_eq!(
        evaluation.lines[0].items[0].gross.as_ref().unwrap().exact,
        "10"
    );
    let line_rows = &evaluation.lines[0].rows;
    assert_eq!(
        row(line_rows, ChargeKind::Recurring)
            .gross
            .as_ref()
            .unwrap()
            .exact,
        "60"
    );
    assert_eq!(
        row(line_rows, ChargeKind::OneTime)
            .gross
            .as_ref()
            .unwrap()
            .exact,
        "5"
    );
    let tcv = evaluation.tcv.unwrap();
    assert_eq!(tcv.amount.exact, "65");
    assert_eq!(tcv.basis, AmountBasis::FiniteTerm);
}

#[test]
fn rolling_years_count_once_and_terms_of_two_kinds_are_mixed() {
    let mut yearly = recurring(id(0x1_0003), flat("120"), "1");
    yearly.binding.recurring_period = Some(RecurringPeriod::Year);
    let input = order(vec![
        line(1, OrderTerm::Rolling, RecurringPeriod::Year, vec![yearly]),
        line(
            2,
            OrderTerm::FixedPeriods { count: periods(2) },
            RecurringPeriod::Month,
            vec![recurring(item(), flat("10"), "1")],
        ),
    ]);
    let evaluation = evaluate_order(&input).unwrap();
    let tcv = evaluation.tcv.unwrap();
    assert_eq!(tcv.amount.exact, "140");
    assert_eq!(tcv.basis, AmountBasis::Mixed);
    // Two cycles stay apart: never one unlabelled per-period sum.
    let cycles: Vec<(RecurringPeriod, &str)> = row(&evaluation.order, ChargeKind::Recurring)
        .recurring_by_cycle
        .iter()
        .map(|cycle| (cycle.cycle, cycle.gross.exact.as_str()))
        .collect();
    assert_eq!(
        cycles,
        [
            (RecurringPeriod::Month, "10"),
            (RecurringPeriod::Year, "120")
        ]
    );
}

#[test]
fn the_basis_is_that_of_the_lines_with_recurring_amounts() {
    // A rolling monthly 10 and a three-month line with only a one-time 5: the recurring amounts
    // are all annualized, so the basis is rolling_annualized, not mixed.
    let input = order(vec![
        line(
            1,
            OrderTerm::Rolling,
            RecurringPeriod::Month,
            vec![recurring(item(), flat("10"), "1")],
        ),
        line(
            2,
            OrderTerm::FixedPeriods { count: periods(3) },
            RecurringPeriod::Month,
            vec![one_time(id(0x1_0002), flat("5"))],
        ),
    ]);
    let evaluation = evaluate_order(&input).unwrap();
    let tcv = evaluation.tcv.unwrap();
    assert_eq!(tcv.amount.exact, "125");
    assert_eq!(tcv.basis, AmountBasis::RollingAnnualized);
    assert_eq!(
        row(&evaluation.order, ChargeKind::Recurring).amount_basis,
        Some(AmountBasis::RollingAnnualized)
    );
    // Without recurring amounts the lines' terms decide.
    let one_time_only = order(vec![line(
        1,
        OrderTerm::FixedPeriods { count: periods(3) },
        RecurringPeriod::Month,
        vec![one_time(id(0x1_0002), flat("5"))],
    )]);
    assert_eq!(
        evaluate_order(&one_time_only).unwrap().tcv.unwrap().basis,
        AmountBasis::FiniteTerm
    );
}

#[test]
fn usage_is_rates_flagged_and_excluded_with_its_minimum_fee_reported() {
    let mut metered = usage(id(0x1_0004));
    metered.binding.price.minimum_fee = Some(decimal("30"));
    let input = order(vec![line(
        1,
        OrderTerm::Rolling,
        RecurringPeriod::Month,
        vec![metered],
    )]);
    let evaluation = evaluate_order(&input).unwrap();
    let item = &evaluation.lines[0].items[0];
    assert_eq!((item.gross.as_ref(), item.net.as_ref()), (None, None));
    assert!(!item.minimum_fee_applied);
    assert_eq!(item.minimum_fee.as_ref().unwrap().exact, "30");
    let rates = item.usage_rates.as_ref().unwrap();
    assert_eq!(rates.rating_window, RatingWindowKind::CalendarHourUtc);
    let usage_row = row(&evaluation.order, ChargeKind::Usage);
    assert_eq!(usage_row.amount_status, AmountStatus::UncommittedUsage);
    assert_eq!(usage_row.gross, None);
    // A usage-only order still has its recurring carrier, and a zero TCV.
    assert_eq!(
        row(&evaluation.order, ChargeKind::Recurring)
            .gross
            .as_ref()
            .unwrap()
            .exact,
        "0"
    );
    assert_eq!(evaluation.tcv.unwrap().amount.exact, "0");
}

#[test]
fn a_minimum_fee_floors_a_recurring_cycle_and_a_one_time_charge() {
    let mut floored = recurring(item(), per_unit("10"), "1");
    floored.binding.price.minimum_fee = Some(decimal("30"));
    let mut once = one_time(id(0x1_0002), flat("5"));
    once.binding.price.minimum_fee = Some(decimal("8"));
    let input = order(vec![line(
        1,
        OrderTerm::FixedPeriods { count: periods(2) },
        RecurringPeriod::Month,
        vec![floored, once],
    )]);
    let evaluation = evaluate_order(&input).unwrap();
    let items = &evaluation.lines[0].items;
    assert_eq!(items[0].rated.as_ref().unwrap().exact, "10");
    assert_eq!(items[0].gross.as_ref().unwrap().exact, "30");
    assert!(items[0].minimum_fee_applied);
    assert_eq!(items[1].gross.as_ref().unwrap().exact, "8");
    assert_eq!(evaluation.tcv.unwrap().amount.exact, "68");
}

#[test]
fn a_preview_line_without_its_term_or_cycle_withholds_only_the_tcv() {
    let mut input = order(vec![
        line(
            1,
            OrderTerm::Missing,
            RecurringPeriod::Month,
            vec![recurring(item(), flat("10"), "1")],
        ),
        line(
            2,
            OrderTerm::Rolling,
            RecurringPeriod::Month,
            vec![one_time(id(0x1_0002), flat("5"))],
        ),
    ]);
    input.lines[1].cycle = None;
    let evaluation = evaluate_order(&input).unwrap();
    assert_eq!(evaluation.tcv, None);
    assert_eq!(evaluation.tcv_withheld, [id(0xA001), id(0xA002)]);
    let first = &evaluation.lines[0];
    assert_eq!(first.horizon, None);
    let recurring = row(&first.rows, ChargeKind::Recurring);
    assert_eq!(
        (recurring.gross.as_ref(), recurring.amount_basis),
        (None, None)
    );
    assert_eq!(recurring.recurring_by_cycle[0].gross.exact, "10");
    assert_eq!(
        row(&evaluation.order, ChargeKind::OneTime)
            .gross
            .as_ref()
            .unwrap()
            .exact,
        "5"
    );
    assert_eq!(row(&evaluation.order, ChargeKind::Recurring).gross, None);
}

#[test]
fn a_preview_line_without_recurring_items_keeps_the_known_recurring_aggregate() {
    // Line 1: 10 a month over 12 periods, a recurring aggregate of 120. Lines 2 and 3 have no
    // recurring item and no cycle: they withhold the TCV, but add a known zero to the recurring
    // aggregate (DESIGN §4.10: every other figure is still returned).
    let mut input = order(vec![
        line(
            1,
            OrderTerm::FixedPeriods { count: periods(12) },
            RecurringPeriod::Month,
            vec![recurring(item(), flat("10"), "1")],
        ),
        line(
            2,
            OrderTerm::Missing,
            RecurringPeriod::Month,
            vec![one_time(id(0x1_0002), flat("5"))],
        ),
        line(
            3,
            OrderTerm::Rolling,
            RecurringPeriod::Month,
            vec![usage(id(0x1_0003))],
        ),
    ]);
    input.lines[1].cycle = None;
    input.lines[2].cycle = None;
    let evaluation = evaluate_order(&input).unwrap();
    assert_eq!(evaluation.tcv, None);
    assert_eq!(evaluation.tcv_withheld, [id(0xA002), id(0xA003)]);
    assert_eq!(evaluation.lines[1].horizon, None);
    let recurring = row(&evaluation.order, ChargeKind::Recurring);
    assert_eq!(pair(recurring.gross.as_ref().unwrap()), ("120", "120"));
    assert_eq!(recurring.amount_basis, Some(AmountBasis::FiniteTerm));
    let one_time = row(&evaluation.order, ChargeKind::OneTime);
    assert_eq!(one_time.gross.as_ref().unwrap().exact, "5");
}

#[test]
fn the_same_recurring_binding_gives_the_amount_evaluate_rates_for_a_whole_period() {
    let model = per_unit("0.015");
    let rated = crate::evaluate(&period_line(model.clone(), "12")).unwrap();
    let input = order(vec![line(
        1,
        OrderTerm::Rolling,
        RecurringPeriod::Month,
        vec![OrderItem {
            item_id: item(),
            quantity: quantity("12"),
            binding: binding(item(), price_id(), model, eur()),
        }],
    )]);
    let evaluation = evaluate_order(&input).unwrap();
    let line = &rated.lines[0].exact_amount;
    assert_eq!(
        (line.numerator.as_str(), line.denominator.as_str()),
        ("9", "50")
    );
    assert_eq!(
        evaluation.lines[0].items[0].gross.as_ref().unwrap().exact,
        "0.18"
    );
}

#[test]
fn an_order_outside_the_sale_profile_fails_by_name() {
    let base = || {
        order(vec![line(
            1,
            OrderTerm::Rolling,
            RecurringPeriod::Month,
            vec![recurring(item(), flat("10"), "1")],
        )])
    };
    let mut input = base();
    input.contract_ref = Some("contract-7".to_owned());
    assert_eq!(reason(&input), "adjustment_unsupported");
    let mut input = base();
    input.currency = "USD".to_owned();
    assert_eq!(reason(&input), "fx_not_supported");
    let mut input = base();
    let mut kwd_scale = recurring(id(0x1_0002), flat("1"), "1");
    kwd_scale.binding.price.currency.scale = 3;
    input.lines[0].items.push(kwd_scale);
    assert_eq!(reason(&input), "binding_currency_mismatch");
    let mut input = base();
    input.lines[0].items[0].binding.price.model = CorePriceModel::Package {
        package_size: decimal("10"),
        package_price: decimal("1"),
    };
    assert_eq!(reason(&input), "unsupported_primitive");
    let mut input = base();
    input.lines[0].cycle = Some(RecurringPeriod::Year);
    assert_eq!(reason(&input), "period_geometry_mismatch");
    // A term of zero periods does not decode at all.
    assert!(serde_json::from_str::<OrderTerm>(r#"{"kind":"fixed_periods","count":0}"#).is_err());
    let mut input = base();
    let twin = input.lines[0].items[0].clone();
    input.lines[0].items.push(twin);
    assert_eq!(reason(&input), "non_injective_mapping");
    let mut input = base();
    input.lines[0].items[0].binding.price.state = PriceState::Cancelled;
    assert_eq!(reason(&input), "price_uncovered");
    let mut input = base();
    input.lines[0].items[0].binding.item_id = id(0x1_0009);
    assert_eq!(reason(&input), "binding_mismatch");
    let mut input = base();
    input.lines[0].items[0].quantity = quantity("-1");
    input.lines[0].items[0].binding.price.model = per_unit("1");
    assert_eq!(reason(&input), "negative_window_quantity");
    let mut input = base();
    input.engine.engine_generation = 2;
    assert_eq!(reason(&input), "engine_unavailable");
    let mut input = base();
    input.lines[0].items.clear();
    assert_eq!(reason(&input), "unsupported_primitive");
    let mut input = base();
    input.lines.clear();
    assert_eq!(reason(&input), "unsupported_primitive");
    // 10^27 × 10^5 needs 33 significant digits: no decimal text holds it.
    let mut input = base();
    input.lines[0].items[0].binding.price.model = per_unit(&format!("1{}", "0".repeat(27)));
    input.lines[0].items[0].quantity = quantity("100000");
    assert_eq!(reason(&input), "precision_overflow");
}

#[test]
fn display_estimates_round_half_to_even() {
    for (amount, display) in [("0.125", "0.12"), ("0.135", "0.14"), ("0.145", "0.14")] {
        let input = order(vec![line(
            1,
            OrderTerm::FixedPeriods { count: periods(1) },
            RecurringPeriod::Month,
            vec![recurring(item(), flat(amount), "1")],
        )]);
        let evaluation = evaluate_order(&input).unwrap();
        let gross = evaluation.lines[0].items[0].gross.as_ref().unwrap();
        assert_eq!(pair(gross), (amount, display));
    }
}

#[test]
fn another_generation_digest_or_budget_is_engine_unavailable() {
    let base = || {
        order(vec![line(
            1,
            OrderTerm::Rolling,
            RecurringPeriod::Month,
            vec![recurring(item(), flat("10"), "1")],
        )])
    };
    let mut input = base();
    input.engine.engine_digest = "00".repeat(32);
    assert_eq!(reason(&input), "engine_unavailable");
    let mut input = base();
    input.budget.scratch_digits = 1_000;
    assert_eq!(reason(&input), "engine_unavailable");
}

#[test]
fn a_currency_bss_money_refuses_is_refused() {
    let base = || {
        order(vec![line(
            1,
            OrderTerm::Rolling,
            RecurringPeriod::Month,
            vec![recurring(item(), flat("10"), "1")],
        )])
    };
    let mut input = base();
    input.lines[0].items[0].binding.price.currency.scale = 200;
    assert_eq!(reason(&input), "binding_currency_mismatch");
    let mut input = base();
    input.currency = "eur ".to_owned();
    assert_eq!(reason(&input), "binding_currency_mismatch");
}

#[test]
fn a_period_on_the_wrong_charge_kind_is_refused() {
    let mut no_period = recurring(item(), flat("10"), "1");
    no_period.binding.recurring_period = None;
    let input = order(vec![line(
        1,
        OrderTerm::Rolling,
        RecurringPeriod::Month,
        vec![no_period],
    )]);
    assert_eq!(reason(&input), "unsupported_primitive");
    let mut once = one_time(id(0x1_0002), flat("5"));
    once.binding.recurring_period = Some(RecurringPeriod::Month);
    let input = order(vec![line(
        1,
        OrderTerm::Rolling,
        RecurringPeriod::Month,
        vec![once],
    )]);
    assert_eq!(reason(&input), "period_geometry_mismatch");
    let mut metered = usage(id(0x1_0004));
    metered.binding.recurring_period = Some(RecurringPeriod::Month);
    let input = order(vec![line(
        1,
        OrderTerm::Rolling,
        RecurringPeriod::Month,
        vec![metered],
    )]);
    assert_eq!(reason(&input), "period_geometry_mismatch");
}

#[test]
fn a_minimum_fee_equal_to_the_amount_is_not_applied() {
    let mut item_at_fee = recurring(item(), per_unit("10"), "3");
    item_at_fee.binding.price.minimum_fee = Some(decimal("30"));
    let input = order(vec![line(
        1,
        OrderTerm::FixedPeriods { count: periods(1) },
        RecurringPeriod::Month,
        vec![item_at_fee],
    )]);
    let evaluation = evaluate_order(&input).unwrap();
    let item = &evaluation.lines[0].items[0];
    assert!(!item.minimum_fee_applied);
    assert_eq!(item.gross.as_ref().unwrap().exact, "30");
}
