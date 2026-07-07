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
        currency: currency.to_owned(),
        balance_minor: balance,
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
    );
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
    let out = ar_aging(&rows, today, &AgingThresholds::default());

    // A/USD/1-30 sums the two same-bucket invoices.
    let a_usd_1_30 = out
        .iter()
        .find(|b| b.payer_tenant_id == payer_a && b.currency == "USD" && b.bucket == "1-30")
        .expect("A USD 1-30 bucket present");
    assert_eq!(
        a_usd_1_30.amount_minor, 1500,
        "same payer+currency+bucket sums"
    );

    // A/EUR is a separate currency grain.
    assert!(
        out.iter()
            .any(|b| b.payer_tenant_id == payer_a && b.currency == "EUR" && b.bucket == "90+"),
        "EUR ages independently of USD"
    );

    // B/USD/current is a separate payer grain.
    assert!(
        out.iter().any(|b| b.payer_tenant_id == payer_b
            && b.currency == "USD"
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
    let out = ar_aging(&rows, today, &AgingThresholds::default());
    assert_eq!(out.len(), 1, "only the positive-balance row ages");
    assert_eq!(out[0].amount_minor, 800);
    assert_eq!(out[0].bucket, "31-60");
}

#[test]
fn empty_input_yields_no_buckets() {
    assert!(ar_aging(&[], naive(2026, 6, 30), &AgingThresholds::default()).is_empty());
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
    let out = ar_aging(&rows, today, &thresholds);
    assert!(
        out.iter()
            .any(|b| b.bucket == "1-15" && b.amount_minor == 100),
        "10 days falls in the 1-15 bucket"
    );
    assert!(
        out.iter()
            .any(|b| b.bucket == "16-45" && b.amount_minor == 200),
        "20 days falls in the 16-45 bucket"
    );
    assert!(
        out.iter()
            .any(|b| b.bucket == "45+" && b.amount_minor == 300),
        "60 days falls in the open-ended 45+ bucket"
    );
}
