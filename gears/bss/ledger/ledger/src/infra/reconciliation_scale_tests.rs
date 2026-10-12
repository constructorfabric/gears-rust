//! Per-currency scale in the X4 tolerance, the untrusted classification of
//! mixed metadata, and the parked v1 event's single variance integer.
#![allow(clippy::unwrap_used)]

use bss_ledger_sdk::{CurrencySpec, PostedMoney, canonical_decimal, parse_decimal};
use uuid::Uuid;

use super::{ar_tolerance_eval, v1_variance_minor};
use crate::domain::error::DomainError;
use crate::domain::reconciliation::{GrainAmount, ReconciliationVariance};
use crate::infra::jobs::tieout::{AccountBalanceVariance, TieOutReport};

fn money(amount: &str, currency: &str, scale: u8) -> PostedMoney {
    PostedMoney::try_new(
        parse_decimal(amount).unwrap(),
        CurrencySpec::try_new(currency.to_owned(), scale).unwrap(),
    )
    .unwrap()
}

fn grain(amount: &str, currency: &str, scale: u8) -> GrainAmount {
    GrainAmount::from_posted(&money(amount, currency, scale))
}

fn variance(computed: GrainAmount, cached: GrainAmount) -> AccountBalanceVariance {
    AccountBalanceVariance {
        account_id: Uuid::now_v7(),
        currency: computed.currency().to_owned(),
        computed,
        cached,
    }
}

/// A report with only account-balance variances and `lines` posted lines.
fn report(lines: u64, variances: Vec<AccountBalanceVariance>) -> TieOutReport {
    TieOutReport {
        tenant_id: Uuid::now_v7(),
        posted_line_count: lines,
        account_balance_variances: variances,
        sub_grain_variances: vec![],
        imbalanced_entries: vec![],
        negative_grains: vec![],
        payment_counter_variances: vec![],
        pending_lines: 0,
    }
}

fn buckets(variance: &ReconciliationVariance) -> Vec<String> {
    match variance {
        ReconciliationVariance::Money { by_currency } => by_currency
            .iter()
            .map(|m| {
                format!(
                    "{} {}@{}",
                    canonical_decimal(m.amount()),
                    m.currency().code(),
                    m.currency().scale()
                )
            })
            .collect(),
        ReconciliationVariance::MissingInvoices { count } => vec![format!("missing={count}")],
    }
}

#[test]
fn each_currency_is_held_to_its_own_scale_budget() {
    // 2 increments per 1,000 lines, 1,000 lines: JPY@0 may drift 2, KWD@3 0.002.
    let at_budget = report(
        1_000,
        vec![
            variance(grain("102", "JPY", 0), grain("100", "JPY", 0)),
            variance(grain("1.002", "KWD", 3), grain("1", "KWD", 3)),
        ],
    );
    let (v, within) = ar_tolerance_eval(&at_budget, 2).unwrap();
    assert_eq!(buckets(&v), vec!["2 JPY@0", "0.002 KWD@3"]);
    assert!(within, "each bucket at its own budget is within tolerance");

    for over in [
        variance(grain("103", "JPY", 0), grain("100", "JPY", 0)),
        variance(grain("1.003", "KWD", 3), grain("1", "KWD", 3)),
    ] {
        let label = format!(
            "{}@{}",
            over.computed.currency(),
            over.computed.currency_scale()
        );
        let (_, within) = ar_tolerance_eval(&report(1_000, vec![over]), 2).unwrap();
        assert!(
            !within,
            "{label}: one increment over its budget blocks close"
        );
    }
    // A fixed 0.01 budget would let 0.009 KWD through and stop 1 JPY; the
    // scale-derived budget does the opposite.
    let (_, within) = ar_tolerance_eval(
        &report(
            1_000,
            vec![variance(grain("1.009", "KWD", 3), grain("1", "KWD", 3))],
        ),
        2,
    )
    .unwrap();
    assert!(!within);
}

#[test]
fn computed_and_cached_metadata_disagreement_is_a_hard_defect() {
    for cached in [grain("100", "JPY", 2), grain("100", "USD", 0)] {
        let (v, within) = ar_tolerance_eval(
            &report(
                1_000,
                vec![variance(grain("100", "JPY", 0), cached.clone())],
            ),
            1_000,
        )
        .unwrap();
        assert!(
            !within,
            "{cached:?}: mismatched metadata never sums into a bucket"
        );
        assert!(buckets(&v).is_empty(), "{:?}", buckets(&v));
    }
}

#[test]
fn one_currency_at_two_scales_is_a_hard_defect_and_keeps_the_first_bucket() {
    let (v, within) = ar_tolerance_eval(
        &report(
            1_000,
            vec![
                variance(grain("1.01", "EUR", 2), grain("1", "EUR", 2)),
                variance(grain("1.001", "EUR", 3), grain("1", "EUR", 3)),
            ],
        ),
        1_000,
    )
    .unwrap();
    assert!(!within, "mixed-scale magnitudes are never summed");
    assert_eq!(buckets(&v), vec!["0.01 EUR@2"]);
}

#[test]
fn v1_variance_carries_the_first_non_zero_bucket_or_the_count() {
    let by_currency =
        |items: Vec<PostedMoney>| ReconciliationVariance::Money { by_currency: items };
    assert_eq!(v1_variance_minor(&by_currency(vec![]), &"run").unwrap(), 0);
    assert_eq!(
        v1_variance_minor(
            &by_currency(vec![money("0", "EUR", 2), money("1.23", "USD", 2)]),
            &"run"
        )
        .unwrap(),
        123,
        "a leading zero bucket is skipped"
    );
    assert_eq!(
        v1_variance_minor(
            &by_currency(vec![money("5", "JPY", 0), money("1.23", "USD", 2)]),
            &"run"
        )
        .unwrap(),
        5,
        "only the first non-zero currency is carried"
    );
    assert_eq!(
        v1_variance_minor(
            &ReconciliationVariance::MissingInvoices { count: 7 },
            &"run"
        )
        .unwrap(),
        7
    );
    // Beyond i64 minor units the parked field saturates; the run still records.
    assert_eq!(
        v1_variance_minor(
            &by_currency(vec![money("9999999999999999999999999999", "USD", 2)]),
            &"run"
        )
        .unwrap(),
        i64::MAX
    );
    assert!(matches!(
        v1_variance_minor(
            &ReconciliationVariance::MissingInvoices { count: u64::MAX },
            &"run"
        ),
        Err(DomainError::Internal(_))
    ));
}
