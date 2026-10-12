//! Exact-sum checks for [`super::ar_aging`]: the per-grain `i128` coefficient sum
//! at the pinned scale, the bounded final total, and amounts whose decimal form
//! carries fewer fractional digits than the currency scale.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::domain::invoice::policy::AgingThresholds;

fn usd(amount: Decimal, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        amount,
        CurrencySpec::try_new("USD".to_owned(), scale).unwrap(),
    )
    .unwrap()
}

fn open_row(payer: Uuid, balance: PostedMoney) -> ArInvoiceBalanceView {
    ArInvoiceBalanceView {
        payer_tenant_id: payer,
        account_id: Uuid::now_v7(),
        invoice_id: "INV".to_owned(),
        balance,
        due_date: None,
    }
}

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 6, 30).unwrap()
}

#[test]
fn amounts_with_fewer_digits_than_the_scale_sum_exactly_at_the_scale() {
    let payer = Uuid::now_v7();
    // 12 (no fraction), 0.5 (one digit), 0.125 (full scale 3) at USD/3.
    let rows = [
        open_row(payer, usd(Decimal::new(12, 0), 3)),
        open_row(payer, usd(Decimal::new(5, 1), 3)),
        open_row(payer, usd(Decimal::new(125, 3), 3)),
    ];
    let out = ar_aging(&rows, today(), &AgingThresholds::default()).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].amount.amount(), Decimal::new(12_625, 3));
    assert_eq!(out[0].amount.currency().scale(), 3);
    assert_eq!(out[0].amount.currency().code(), "USD");
}

#[test]
fn a_bucket_total_past_the_money_bound_is_a_range_error_not_a_truncation() {
    let payer = Uuid::now_v7();
    let max = Decimal::from_str_exact("9999999999999999999999999999").unwrap();
    let rows = [
        open_row(payer, usd(max, 0)),
        open_row(payer, usd(Decimal::ONE, 0)),
    ];
    let err = ar_aging(&rows, today(), &AgingThresholds::default()).unwrap_err();
    assert_eq!(err, ExactError::Money(MoneyError::AmountOutOfRange));
}

#[test]
fn the_largest_in_range_total_is_kept_exactly() {
    let payer = Uuid::now_v7();
    let near = Decimal::from_str_exact("99999999999999999999999999.98").unwrap();
    let rows = [
        open_row(payer, usd(near, 2)),
        open_row(payer, usd(Decimal::new(1, 2), 2)),
    ];
    let out = ar_aging(&rows, today(), &AgingThresholds::default()).unwrap();
    assert_eq!(
        out[0].amount.amount(),
        Decimal::from_str_exact("99999999999999999999999999.99").unwrap()
    );
}

/// High-scale balances age exactly: a bucket whose coefficient at scale 28 is
/// past the 96-bit decimal carrier (`5 + 5`), or past `i128` itself (`10^11`
/// per row), is still the exact posting, as the plain exact sum returned it.
#[test]
fn high_scale_buckets_past_the_coefficient_carriers_are_kept_exactly() {
    let payer = Uuid::now_v7();
    let rows = [
        open_row(payer, usd(Decimal::new(5, 0), 28)),
        open_row(payer, usd(Decimal::new(5, 0), 28)),
    ];
    let out = ar_aging(&rows, today(), &AgingThresholds::default()).unwrap();
    assert_eq!(out[0].amount.amount(), Decimal::new(10, 0));
    assert_eq!(out[0].amount.currency().scale(), 28);

    let big = Decimal::from_str_exact("100000000000").unwrap();
    let rows = [open_row(payer, usd(big, 28)), open_row(payer, usd(big, 28))];
    let out = ar_aging(&rows, today(), &AgingThresholds::default()).unwrap();
    assert_eq!(
        out[0].amount.amount(),
        Decimal::from_str_exact("200000000000").unwrap()
    );
}
