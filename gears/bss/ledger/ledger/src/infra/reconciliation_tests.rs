//! Unit tests for the AR↔derived rounding-tolerance evaluation (`ar_tolerance_eval`,
//! the X4 logic) — the correctness-critical part of the reconciliation framework that
//! decides whether a tie-out variance blocks period close.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use uuid::Uuid;

use super::ar_tolerance_eval;
use crate::infra::jobs::tieout::{AccountBalanceVariance, ImbalancedEntry, TieOutReport};

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
    AccountBalanceVariance {
        account_id: Uuid::from_u128(0xB1),
        currency: "USD".to_owned(),
        computed,
        cached,
    }
}

#[test]
fn clean_report_is_zero_variance_within_tolerance() {
    let (variance, within) = ar_tolerance_eval(&clean(5_000), 1);
    assert_eq!(variance, 0);
    assert!(within);
}

#[test]
fn monetary_variance_within_rounding_budget_is_within_tolerance() {
    // 2000 posted lines, 1 minor/1000 → budget 2. A 1-minor divergence fits.
    let mut report = clean(2_000);
    report.account_balance_variances = vec![balance_variance(100, 99)];
    let (variance, within) = ar_tolerance_eval(&report, 1);
    assert_eq!(variance, 1);
    assert!(within, "1 minor <= budget 2");
}

#[test]
fn monetary_variance_exceeding_budget_is_out_of_tolerance() {
    // 2000 posted lines, budget 2. A 5-minor divergence exceeds it.
    let mut report = clean(2_000);
    report.account_balance_variances = vec![balance_variance(100, 95)];
    let (variance, within) = ar_tolerance_eval(&report, 1);
    assert_eq!(variance, 5);
    assert!(!within, "5 minor > budget 2");
}

#[test]
fn small_tenant_gets_the_statutory_floor_budget() {
    // 500 posted lines: 500/1000 = 0 by integer division, but the budget is FLOORED at
    // the statutory minimum (`per_k_lines` = 1 minor) so a sub-1000-line period can still
    // absorb the immaterial-rounding bucket the design grants — a 1-minor divergence is
    // within tolerance instead of spuriously blocking close.
    let mut report = clean(500);
    report.account_balance_variances = vec![balance_variance(100, 99)];
    let (variance, within) = ar_tolerance_eval(&report, 1);
    assert_eq!(variance, 1);
    assert!(within, "1 minor <= statutory floor budget 1");

    // A divergence ABOVE the floor still blocks.
    let mut report = clean(500);
    report.account_balance_variances = vec![balance_variance(100, 97)];
    let (variance, within) = ar_tolerance_eval(&report, 1);
    assert_eq!(variance, 3);
    assert!(!within, "3 minor > statutory floor budget 1");
}

#[test]
fn structural_defect_is_never_within_tolerance_even_at_zero_variance() {
    // An imbalanced entry is a hard defect (not rounding): out of tolerance regardless
    // of the monetary budget, and it carries no netted monetary variance here.
    let mut report = clean(5_000);
    report.imbalanced_entries = vec![ImbalancedEntry {
        entry_id: Uuid::from_u128(0xE1),
        currency: "USD".to_owned(),
        net_minor: 10,
        line_count: 2,
        payer_count: 1,
    }];
    let (variance, within) = ar_tolerance_eval(&report, 1);
    assert_eq!(
        variance, 0,
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
    let (variance, within) = ar_tolerance_eval(&report, 1);
    assert_eq!(variance, 0);
    assert!(!within);
}

#[test]
fn multiple_grain_divergences_sum_in_absolute_value() {
    // Two opposite-sign divergences must NOT net to zero — the tie-out variance is the
    // total absolute divergence (each grain is independently wrong).
    let mut report = clean(2_000);
    report.account_balance_variances = vec![balance_variance(100, 98), balance_variance(50, 52)];
    let (variance, _within) = ar_tolerance_eval(&report, 1);
    assert_eq!(variance, 4, "|+2| + |-2| = 4, not 0");
}
