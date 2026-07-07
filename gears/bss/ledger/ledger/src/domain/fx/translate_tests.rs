//! Tests for the pure FX translation + residual plug.

use super::*;

fn dr(amount: i64) -> FxLine {
    FxLine {
        amount_minor: amount,
        side: Side::Debit,
    }
}

fn cr(amount: i64) -> FxLine {
    FxLine {
        amount_minor: amount,
        side: Side::Credit,
    }
}

/// Asserts the functional column nets to zero (DR == CR) for `lines`/`func`.
fn func_net(lines: &[FxLine], func: &[i64]) -> i128 {
    lines
        .iter()
        .zip(func)
        .map(|(l, &f)| match l.side {
            Side::Debit => i128::from(f),
            Side::Credit => -i128::from(f),
        })
        .sum()
}

#[test]
fn identity_rate_mirrors_transaction() {
    // rate 1.0 → functional == transaction, no residual.
    let lines = [dr(1000), cr(1000)];
    let func = translate_entry(&lines, 1_000_000, 0).unwrap();
    assert_eq!(func, vec![1000, 1000]);
    assert_eq!(func_net(&lines, &func), 0);
}

#[test]
fn scaled_rate_balances_both_columns() {
    // rate 1.1 → DR 1100 / CR 1100, balances.
    let lines = [dr(1000), cr(1000)];
    let func = translate_entry(&lines, 1_100_000, 0).unwrap();
    assert_eq!(func, vec![1100, 1100]);
    assert_eq!(func_net(&lines, &func), 0);
}

#[test]
fn rounding_residual_is_plugged_onto_the_anchor() {
    // rate 1.5: DR 1 → 1.5 → 2 (ties to even), DR 1 → 2, CR 2 → 3.0 → 3.
    // Functional DR 4 vs CR 3 — a residual of 1; the CR anchor (idx 2) absorbs it
    // → 4, so both columns net to zero. The transaction column is exact (DR 2 = CR 2).
    let lines = [dr(1), dr(1), cr(2)];
    let func = translate_entry(&lines, 1_500_000, 2).unwrap();
    assert_eq!(func, vec![2, 2, 4]);
    assert_eq!(
        func_net(&lines, &func),
        0,
        "functional column must net to zero"
    );
}

#[test]
fn residual_plug_is_deterministic() {
    // Same inputs → byte-identical output (no Date/random; banker's rounding).
    let lines = [dr(1), dr(1), cr(2)];
    let a = translate_entry(&lines, 1_500_000, 2).unwrap();
    let b = translate_entry(&lines, 1_500_000, 2).unwrap();
    assert_eq!(a, b);
}

#[test]
fn dr_anchor_absorbs_residual() {
    // Mirror of the CR-anchor case with the residual pushed onto a DR anchor.
    // rate 1.5: CR 1 → 2, CR 1 → 2, DR 2 → 3. Functional DR 3 vs CR 4 → net −1;
    // the DR anchor (idx 2) absorbs +1 → 4, both columns net to zero.
    let lines = [cr(1), cr(1), dr(2)];
    let func = translate_entry(&lines, 1_500_000, 2).unwrap();
    assert_eq!(func, vec![2, 2, 4]);
    assert_eq!(func_net(&lines, &func), 0);
}

#[test]
fn non_positive_rate_is_rejected() {
    let lines = [dr(1000), cr(1000)];
    assert_eq!(
        translate_entry(&lines, 0, 0),
        Err(FxTranslateError::RateNonPositive)
    );
}

#[test]
fn translate_amount_rejects_non_positive_rate() {
    // The single-amount path (used by the unrealized-revaluation run) must reject
    // a `<= 0` rate too — a zero rate would zero out the position and a negative
    // rate would flip its sign, posting a wrong FX entry instead of erroring.
    assert_eq!(
        translate_amount(10_000, 0),
        Err(FxTranslateError::RateNonPositive)
    );
    assert_eq!(
        translate_amount(10_000, -1_100_000),
        Err(FxTranslateError::RateNonPositive)
    );
    // A valid positive rate still translates ($100.00 × 1.1 = $110.00).
    assert_eq!(translate_amount(10_000, 1_100_000), Ok(11_000));
}

#[test]
fn anchor_out_of_bounds_is_rejected() {
    let lines = [dr(1000), cr(1000)];
    assert_eq!(
        translate_entry(&lines, 1_000_000, 2),
        Err(FxTranslateError::AnchorOutOfBounds)
    );
}
