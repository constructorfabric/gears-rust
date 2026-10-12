//! Unit tests for the AR↔derived rounding-tolerance evaluation (`ar_tolerance_eval`,
//! the X4 logic) — the correctness-critical part of the reconciliation framework that
//! decides whether a tie-out variance blocks period close.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use bss_ledger_sdk::{CurrencySpec, PostedMoney, canonical_decimal};
use rust_decimal::Decimal;
use uuid::Uuid;

use super::ar_tolerance_eval;
use crate::domain::reconciliation::{GrainAmount, ReconciliationVariance};
use crate::infra::jobs::tieout::{AccountBalanceVariance, ImbalancedEntry, TieOutReport};

/// A scale-2 posting from a cent count in `currency`.
fn cents(currency: &str, minor: i64) -> PostedMoney {
    PostedMoney::try_new(
        Decimal::new(minor, 2),
        CurrencySpec::try_new(currency.to_owned(), 2).expect("spec"),
    )
    .expect("posting")
}

fn ga(currency: &str, minor: i64) -> GrainAmount {
    GrainAmount::from_posted(&cents(currency, minor))
}

/// The variance rendered as `"<amount> <code>"` per bucket, in order.
fn buckets(variance: &ReconciliationVariance) -> Vec<String> {
    match variance {
        ReconciliationVariance::Money { by_currency } => by_currency
            .iter()
            .map(|m| format!("{} {}", canonical_decimal(m.amount()), m.currency().code()))
            .collect(),
        ReconciliationVariance::MissingInvoices { count } => vec![format!("missing={count}")],
    }
}

/// A clean report (no defects) with the given posted-line count.
fn clean(posted_line_count: u64) -> TieOutReport {
    TieOutReport {
        tenant_id: Uuid::from_u128(0xA1),
        posted_line_count,
        account_balance_variances: vec![],
        sub_grain_variances: vec![],
        imbalanced_entries: vec![],
        negative_grains: vec![],
        payment_counter_variances: vec![],
        pending_lines: 0,
    }
}

fn balance_variance(computed: i64, cached: i64) -> AccountBalanceVariance {
    balance_variance_in("USD", computed, cached)
}

fn balance_variance_in(currency: &str, computed: i64, cached: i64) -> AccountBalanceVariance {
    AccountBalanceVariance {
        account_id: Uuid::from_u128(0xB1),
        currency: currency.to_owned(),
        computed: ga(currency, computed),
        cached: ga(currency, cached),
    }
}

#[test]
fn clean_report_is_zero_variance_within_tolerance() {
    let (variance, within) = ar_tolerance_eval(&clean(5_000), 1).expect("eval");
    assert!(variance.is_zero());
    assert!(
        buckets(&variance).is_empty(),
        "a clean report has no bucket"
    );
    assert!(within);
}

#[test]
fn monetary_variance_within_rounding_budget_is_within_tolerance() {
    // 2000 posted lines, 1 minor/1000 → budget 2. A 1-minor divergence fits.
    let mut report = clean(2_000);
    report.account_balance_variances = vec![balance_variance(100, 99)];
    let (variance, within) = ar_tolerance_eval(&report, 1).expect("eval");
    assert_eq!(buckets(&variance), vec!["0.01 USD"]);
    assert!(within, "1 increment <= budget 2");
}

#[test]
fn monetary_variance_exceeding_budget_is_out_of_tolerance() {
    // 2000 posted lines, budget 2. A 5-minor divergence exceeds it.
    let mut report = clean(2_000);
    report.account_balance_variances = vec![balance_variance(100, 95)];
    let (variance, within) = ar_tolerance_eval(&report, 1).expect("eval");
    assert_eq!(buckets(&variance), vec!["0.05 USD"]);
    assert!(!within, "5 increments > budget 2");
}

#[test]
fn small_tenant_gets_the_statutory_floor_budget() {
    // 500 posted lines: 500/1000 = 0 by integer division, but the budget is FLOORED at
    // the statutory minimum (`per_k_lines` = 1 minor) so a sub-1000-line period can still
    // absorb the immaterial-rounding bucket the design grants — a 1-minor divergence is
    // within tolerance instead of spuriously blocking close.
    let mut report = clean(500);
    report.account_balance_variances = vec![balance_variance(100, 99)];
    let (variance, within) = ar_tolerance_eval(&report, 1).expect("eval");
    assert_eq!(buckets(&variance), vec!["0.01 USD"]);
    assert!(within, "1 increment <= statutory floor budget 1");

    // A divergence ABOVE the floor still blocks.
    let mut report = clean(500);
    report.account_balance_variances = vec![balance_variance(100, 97)];
    let (variance, within) = ar_tolerance_eval(&report, 1).expect("eval");
    assert_eq!(buckets(&variance), vec!["0.03 USD"]);
    assert!(!within, "3 increments > statutory floor budget 1");
}

#[test]
fn structural_defect_is_never_within_tolerance_even_at_zero_variance() {
    // An imbalanced entry is a hard defect (not rounding): out of tolerance regardless
    // of the monetary budget, and it carries no netted monetary variance here.
    let mut report = clean(5_000);
    report.imbalanced_entries = vec![ImbalancedEntry {
        entry_id: Uuid::from_u128(0xE1),
        currency: "USD".to_owned(),
        net: ga("USD", 10),
        line_count: 2,
        payer_count: 1,
    }];
    let (variance, within) = ar_tolerance_eval(&report, 1).expect("eval");
    assert!(
        variance.is_zero(),
        "imbalance is not a netted balance-cache divergence"
    );
    assert!(!within, "a hard defect is never within rounding tolerance");
}

#[test]
fn pending_mapping_lines_block_even_with_no_variance() {
    // PENDING suspense lines (mapping gap) make the report not-clean and are a hard
    // defect — out of tolerance with zero monetary variance.
    let mut report = clean(5_000);
    report.pending_lines = 3;
    let (variance, within) = ar_tolerance_eval(&report, 1).expect("eval");
    assert!(variance.is_zero());
    assert!(!within);
}

#[test]
fn multiple_grain_divergences_sum_in_absolute_value() {
    // Two opposite-sign divergences must NOT net to zero — the tie-out variance is the
    // total absolute divergence (each grain is independently wrong).
    let mut report = clean(2_000);
    report.account_balance_variances = vec![balance_variance(100, 98), balance_variance(50, 52)];
    let (variance, _within) = ar_tolerance_eval(&report, 1).expect("eval");
    assert_eq!(
        buckets(&variance),
        vec!["0.04 USD"],
        "|+2| + |-2| = 4, not 0"
    );
}

/// Buckets never mix currencies: each currency is summed and judged on its own
/// budget, in deterministic currency order; the run passes only when every
/// bucket passes.
#[test]
fn mixed_currency_variances_are_bucketed_and_judged_per_currency() {
    // 2000 lines ⇒ budget 2 increments per currency.
    let mut report = clean(2_000);
    report.account_balance_variances = vec![
        balance_variance_in("USD", 100, 99),
        balance_variance_in("JPY", 500, 499),
        balance_variance_in("USD", 10, 9),
    ];
    let (variance, within) = ar_tolerance_eval(&report, 1).expect("eval");
    assert_eq!(
        buckets(&variance),
        vec!["0.01 JPY", "0.02 USD"],
        "one bucket per currency, currency order, never summed across"
    );
    assert!(within, "both buckets fit their own budget");

    // One currency over budget fails the run even though the other is clean.
    let mut report = clean(2_000);
    report.account_balance_variances = vec![
        balance_variance_in("USD", 100, 99),
        balance_variance_in("EUR", 100, 50),
    ];
    let (variance, within) = ar_tolerance_eval(&report, 1).expect("eval");
    assert_eq!(buckets(&variance), vec!["0.5 EUR", "0.01 USD"]);
    assert!(!within, "the EUR bucket breaches its budget");
}

/// A total that cannot be trusted (corrupt stored text / arithmetic budget) is
/// a hard defect, never a rounding variance.
#[test]
fn untrusted_total_is_a_hard_defect() {
    let mut report = clean(5_000);
    let mut v = balance_variance(100, 100);
    v.cached.mark_untrusted();
    report.account_balance_variances = vec![v];
    let (_, within) = ar_tolerance_eval(&report, 1).expect("eval");
    assert!(!within);
}
