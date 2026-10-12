//! Tests for the AR-aging bucket derivation ([`super::ar_aging`]).

use super::*;
use crate::domain::invoice::policy::AgingThresholds;

fn naive(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn row(payer: Uuid, currency: &str, balance: i64, due: Option<NaiveDate>) -> ArInvoiceBalanceView {
    ArInvoiceBalanceView {
        payer_tenant_id: payer,
        account_id: Uuid::now_v7(),
        invoice_id: format!("INV-{balance}"),
        balance: PostedMoney::try_new(
            Decimal::new(balance, 2),
            CurrencySpec::try_new(currency.to_owned(), 2).unwrap(),
        )
        .unwrap(),
        due_date: due,
    }
}

/// Find the bucket label for a single-row aging over `due` as of `today`, under
/// the default thresholds (`[30, 60, 90]`).
fn bucket_for(due: Option<NaiveDate>, today: NaiveDate) -> Option<String> {
    let payer = Uuid::now_v7();
    let out = ar_aging(
        &[row(payer, "USD", 1000, due)],
        today,
        &AgingThresholds::default(),
    )
    .unwrap();
    out.first().map(|b| b.bucket.clone())
}

#[test]
fn bucket_boundaries_are_inclusive_at_the_documented_days() {
    let today = naive(2026, 6, 30);
    // 0 days past due (due == today) ⇒ current.
    assert_eq!(
        bucket_for(Some(today), today).as_deref(),
        Some(BUCKET_CURRENT)
    );
    // Future due date ⇒ current.
    assert_eq!(
        bucket_for(Some(naive(2026, 7, 15)), today).as_deref(),
        Some(BUCKET_CURRENT)
    );
    // No due date ⇒ current.
    assert_eq!(bucket_for(None, today).as_deref(), Some(BUCKET_CURRENT));
    // 1 day past due ⇒ 1-30; 30 days ⇒ 1-30.
    assert_eq!(
        bucket_for(Some(naive(2026, 6, 29)), today).as_deref(),
        Some("1-30")
    );
    assert_eq!(
        bucket_for(Some(naive(2026, 5, 31)), today).as_deref(),
        Some("1-30")
    );
    // 31 days ⇒ 31-60; 60 days ⇒ 31-60.
    assert_eq!(
        bucket_for(Some(naive(2026, 5, 30)), today).as_deref(),
        Some("31-60")
    );
    assert_eq!(
        bucket_for(Some(naive(2026, 5, 1)), today).as_deref(),
        Some("31-60")
    );
    // 61 days ⇒ 61-90; 90 days ⇒ 61-90.
    assert_eq!(
        bucket_for(Some(naive(2026, 4, 30)), today).as_deref(),
        Some("61-90")
    );
    assert_eq!(
        bucket_for(Some(naive(2026, 4, 1)), today).as_deref(),
        Some("61-90")
    );
    // 91 days ⇒ 90+.
    assert_eq!(
        bucket_for(Some(naive(2026, 3, 31)), today).as_deref(),
        Some("90+")
    );
}

#[test]
fn buckets_separate_per_payer_and_currency() {
    let today = naive(2026, 6, 30);
    let payer_a = Uuid::now_v7();
    let payer_b = Uuid::now_v7();
    let rows = vec![
        // A: two USD invoices in the same bucket (sum), one EUR in another.
        row(payer_a, "USD", 1000, Some(naive(2026, 6, 29))), // 1 day → 1-30
        row(payer_a, "USD", 500, Some(naive(2026, 6, 20))),  // 10 days → 1-30
        row(payer_a, "EUR", 700, Some(naive(2026, 3, 1))),   // 90+
        // B: one USD in current.
        row(payer_b, "USD", 250, None),
    ];
    let out = ar_aging(&rows, today, &AgingThresholds::default()).unwrap();

    // A/USD/1-30 sums the two same-bucket invoices.
    let a_usd_1_30 = out
        .iter()
        .find(|b| {
            b.payer_tenant_id == payer_a
                && b.amount.currency().code() == "USD"
                && b.bucket == "1-30"
        })
        .expect("A USD 1-30 bucket present");
    assert_eq!(
        a_usd_1_30.amount.amount(),
        money(1500).amount(),
        "same payer+currency+bucket sums"
    );

    // A/EUR is a separate currency grain.
    assert!(
        out.iter().any(|b| b.payer_tenant_id == payer_a
            && b.amount.currency().code() == "EUR"
            && b.bucket == "90+"),
        "EUR ages independently of USD"
    );

    // B/USD/current is a separate payer grain.
    assert!(
        out.iter().any(|b| b.payer_tenant_id == payer_b
            && b.amount.currency().code() == "USD"
            && b.bucket == BUCKET_CURRENT),
        "payer B is a separate grain"
    );
}

#[test]
fn zero_and_negative_balances_are_excluded() {
    let today = naive(2026, 6, 30);
    let payer = Uuid::now_v7();
    let rows = vec![
        row(payer, "USD", 0, Some(naive(2026, 5, 1))), // settled
        row(payer, "USD", -300, Some(naive(2026, 5, 1))), // credit
        row(payer, "USD", 800, Some(naive(2026, 5, 1))), // open 60-day
    ];
    let out = ar_aging(&rows, today, &AgingThresholds::default()).unwrap();
    assert_eq!(out.len(), 1, "only the positive-balance row ages");
    assert_eq!(out[0].amount.amount(), money(800).amount());
    assert_eq!(out[0].bucket, "31-60");
}

#[test]
fn empty_input_yields_no_buckets() {
    assert!(
        ar_aging(&[], naive(2026, 6, 30), &AgingThresholds::default())
            .unwrap()
            .is_empty()
    );
}

/// VHP-1853: custom tenant thresholds reshape the buckets AND their labels.
#[test]
fn custom_thresholds_reshape_buckets_and_labels() {
    let today = naive(2026, 6, 30);
    let payer = Uuid::now_v7();
    let thresholds = AgingThresholds::new(vec![15, 45]).expect("valid thresholds");
    let rows = vec![
        row(payer, "USD", 100, Some(naive(2026, 6, 20))), // 10 days → 1-15
        row(payer, "USD", 200, Some(naive(2026, 6, 10))), // 20 days → 16-45
        row(payer, "USD", 300, Some(naive(2026, 5, 1))),  // 60 days → 45+
    ];
    let out = ar_aging(&rows, today, &thresholds).unwrap();
    assert!(
        out.iter()
            .any(|b| b.bucket == "1-15" && b.amount.amount() == money(100).amount()),
        "10 days falls in the 1-15 bucket"
    );
    assert!(
        out.iter()
            .any(|b| b.bucket == "16-45" && b.amount.amount() == money(200).amount()),
        "20 days falls in the 16-45 bucket"
    );
    assert!(
        out.iter()
            .any(|b| b.bucket == "45+" && b.amount.amount() == money(300).amount()),
        "60 days falls in the open-ended 45+ bucket"
    );
}

use bss_ledger_sdk::money::{CurrencySpec, PostedMoney};
use rust_decimal::Decimal;

/// Preserve these legacy scale-2 fixture economics as explicit major-unit money.
fn money(cents: i64) -> PostedMoney {
    PostedMoney::try_new(
        Decimal::new(cents, 2),
        CurrencySpec::try_new("USD".to_owned(), 2).unwrap(),
    )
    .unwrap()
}

#[test]
fn fractional_positive_aging_is_exact_and_nonpositive_rows_stay_skipped() {
    let payer = Uuid::now_v7();
    let rows = [
        row(payer, "EUR", 1234, None),
        row(payer, "EUR", 34, None),
        row(payer, "EUR", -100, None),
        row(payer, "EUR", 0, None),
    ];
    let out = ar_aging(&rows, naive(2026, 6, 30), &AgingThresholds::default()).unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].amount.amount(), Decimal::new(1268, 2));
    assert_eq!(
        out[0].amount.currency(),
        &CurrencySpec::try_new("EUR".to_owned(), 2).unwrap()
    );
}

#[test]
fn scale_conflict_within_payer_currency_is_rejected_even_in_skipped_or_other_age_bucket() {
    let payer = Uuid::now_v7();
    for balance in [0, -1, 1] {
        let mut conflicting = row(payer, "USD", balance, Some(naive(2026, 3, 1)));
        conflicting.balance = PostedMoney::try_new(
            Decimal::new(balance, 3),
            CurrencySpec::try_new("USD".to_owned(), 3).unwrap(),
        )
        .unwrap();
        assert_eq!(
            ar_aging(
                &[row(payer, "USD", 1234, None), conflicting],
                naive(2026, 6, 30),
                &AgingThresholds::default()
            ),
            Err(ExactError::Money(MoneyError::ScaleMismatch))
        );
    }
}

#[test]
fn unrelated_payers_may_have_distinct_stored_scales_without_new_key_axis() {
    let mut other = row(Uuid::now_v7(), "USD", 0, None);
    other.balance = PostedMoney::try_new(
        Decimal::new(1234, 3),
        CurrencySpec::try_new("USD".to_owned(), 3).unwrap(),
    )
    .unwrap();
    let out = ar_aging(
        &[row(Uuid::now_v7(), "USD", 1234, None), other],
        naive(2026, 6, 30),
        &AgingThresholds::default(),
    )
    .unwrap();
    assert_eq!(out.len(), 2);
}

#[test]
fn aging_final_overflow_rejects_instead_of_saturating() {
    let payer = Uuid::now_v7();
    let mut big = row(payer, "USD", 0, None);
    big.balance = PostedMoney::try_new(
        bss_ledger_sdk::money::parse_decimal("9999999999999999999999999999").unwrap(),
        CurrencySpec::try_new("USD".to_owned(), 2).unwrap(),
    )
    .unwrap();
    assert_eq!(
        ar_aging(
            &[big, row(payer, "USD", 100, None)],
            naive(2026, 6, 30),
            &AgingThresholds::default()
        ),
        Err(ExactError::Money(MoneyError::AmountOutOfRange))
    );
}
