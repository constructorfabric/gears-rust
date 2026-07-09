//! Unit tests for the pure unrealized-revaluation remeasure (`revaluation.rs`):
//! sign-by-role across asset (AR) vs liability (UNALLOCATED / `REUSABLE_CREDIT`)
//! grains in both rate directions (gain / loss), the no-movement (Δ == 0) case,
//! a multi-grain same-scope net, the empty run, and the malformed-position
//! errors. Every result is also checked against the functional-column balance
//! invariant (grain legs + the `FX_UNREALIZED` line net to zero).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

/// A `RevaluationPosition` literal.
fn pos(
    normal_side: Side,
    carried_functional_minor: i64,
    remeasured_functional_minor: i64,
) -> RevaluationPosition {
    RevaluationPosition {
        normal_side,
        carried_functional_minor,
        remeasured_functional_minor,
    }
}

/// Assert the revaluation entry's functional column balances: Σ DR (grain legs +
/// the `FX_UNREALIZED` line) == Σ CR. This is the invariant `remeasure` must
/// guarantee by construction (the whole entry is functional-only).
fn assert_functional_balances(r: &Revaluation) {
    let mut dr: i128 = 0;
    let mut cr: i128 = 0;
    for line in r.grain_lines.iter().flatten() {
        match line.side {
            Side::Debit => dr += i128::from(line.functional_minor),
            Side::Credit => cr += i128::from(line.functional_minor),
        }
    }
    if let Some(fx) = r.fx_unrealized {
        match fx.side {
            Side::Debit => dr += i128::from(fx.functional_minor),
            Side::Credit => cr += i128::from(fx.functional_minor),
        }
    }
    assert_eq!(dr, cr, "functional column must balance (DR == CR)");
}

#[test]
fn asset_ar_rate_fall_books_unrealized_loss() {
    // AR (debit-normal asset) carried 132.00 USD, period-end remeasure 126.00:
    // the asset is worth LESS in functional → CR AR 6.00 (carrying down),
    // DR FX_UNREALIZED 6.00 (unrealized loss).
    let positions = [pos(Side::Debit, 13_200, 12_600)];
    let r = remeasure(&positions).unwrap();
    assert_eq!(
        r.grain_lines,
        vec![Some(RevaluationLine {
            side: Side::Credit,
            functional_minor: 600,
        })]
    );
    assert_eq!(
        r.fx_unrealized,
        Some(RevaluationLine {
            side: Side::Debit,
            functional_minor: 600,
        }),
        "net 6.00 USD unrealized LOSS on the DR side"
    );
    assert_functional_balances(&r);
}

#[test]
fn asset_ar_rate_rise_books_unrealized_gain() {
    // AR carried 132.00, remeasure 138.00: asset worth MORE → DR AR 6.00,
    // CR FX_UNREALIZED 6.00 (unrealized gain).
    let positions = [pos(Side::Debit, 13_200, 13_800)];
    let r = remeasure(&positions).unwrap();
    assert_eq!(
        r.grain_lines,
        vec![Some(RevaluationLine {
            side: Side::Debit,
            functional_minor: 600,
        })]
    );
    assert_eq!(
        r.fx_unrealized,
        Some(RevaluationLine {
            side: Side::Credit,
            functional_minor: 600,
        }),
        "net 6.00 USD unrealized GAIN on the CR side"
    );
    assert_functional_balances(&r);
}

#[test]
fn liability_unallocated_rate_rise_books_unrealized_loss() {
    // UNALLOCATED (credit-normal liability) carried 129.60, remeasure 135.00:
    // the liability owed back is worth MORE in functional → CR UNALLOCATED 5.40
    // (carrying up), DR FX_UNREALIZED 5.40 (unrealized loss).
    let positions = [pos(Side::Credit, 12_960, 13_500)];
    let r = remeasure(&positions).unwrap();
    assert_eq!(
        r.grain_lines,
        vec![Some(RevaluationLine {
            side: Side::Credit,
            functional_minor: 540,
        })]
    );
    assert_eq!(
        r.fx_unrealized,
        Some(RevaluationLine {
            side: Side::Debit,
            functional_minor: 540,
        }),
        "net 5.40 USD unrealized LOSS (liability grew)"
    );
    assert_functional_balances(&r);
}

#[test]
fn liability_reusable_credit_rate_fall_books_unrealized_gain() {
    // REUSABLE_CREDIT (credit-normal liability) carried 200.00, remeasure 190.00:
    // the wallet held is worth LESS in functional → DR REUSABLE_CREDIT 10.00
    // (carrying down), CR FX_UNREALIZED 10.00 (unrealized gain).
    let positions = [pos(Side::Credit, 20_000, 19_000)];
    let r = remeasure(&positions).unwrap();
    assert_eq!(
        r.grain_lines,
        vec![Some(RevaluationLine {
            side: Side::Debit,
            functional_minor: 1_000,
        })]
    );
    assert_eq!(
        r.fx_unrealized,
        Some(RevaluationLine {
            side: Side::Credit,
            functional_minor: 1_000,
        }),
        "net 10.00 USD unrealized GAIN (liability shrank)"
    );
    assert_functional_balances(&r);
}

#[test]
fn no_movement_emits_no_line() {
    // Carried == remeasured (closed at the carried rate): no leg, nothing to post.
    let positions = [pos(Side::Debit, 13_200, 13_200)];
    let r = remeasure(&positions).unwrap();
    assert_eq!(r.grain_lines, vec![None]);
    assert_eq!(r.fx_unrealized, None);
    assert!(r.is_empty());
    assert_functional_balances(&r);
}

#[test]
fn multi_grain_same_scope_nets_to_one_fx_line() {
    // Scope = AR (two invoices, both debit-normal), opposite rate moves:
    //   inv1 carried 100.00 → 90.00  : CR AR 10.00 (down)
    //   inv2 carried 200.00 → 230.00 : DR AR 30.00 (up)
    // Net Δ = +30 − 10 = +20 (DR side wins) → CR FX_UNREALIZED 20.00.
    let positions = [
        pos(Side::Debit, 10_000, 9_000),
        pos(Side::Debit, 20_000, 23_000),
    ];
    let r = remeasure(&positions).unwrap();
    assert_eq!(
        r.grain_lines,
        vec![
            Some(RevaluationLine {
                side: Side::Credit,
                functional_minor: 1_000,
            }),
            Some(RevaluationLine {
                side: Side::Debit,
                functional_minor: 3_000,
            }),
        ]
    );
    assert_eq!(
        r.fx_unrealized,
        Some(RevaluationLine {
            side: Side::Credit,
            functional_minor: 2_000,
        }),
        "net 20.00 USD unrealized GAIN"
    );
    assert_functional_balances(&r);
}

#[test]
fn mixed_grains_that_cancel_emit_no_fx_line() {
    // Two AR grains whose moves cancel exactly: +5.00 and −5.00 → net 0, but the
    // per-grain legs still post (carrying values do move) and the entry still
    // balances without an FX line.
    let positions = [
        pos(Side::Debit, 10_000, 10_500),
        pos(Side::Debit, 8_000, 7_500),
    ];
    let r = remeasure(&positions).unwrap();
    assert_eq!(
        r.grain_lines,
        vec![
            Some(RevaluationLine {
                side: Side::Debit,
                functional_minor: 500,
            }),
            Some(RevaluationLine {
                side: Side::Credit,
                functional_minor: 500,
            }),
        ]
    );
    assert_eq!(r.fx_unrealized, None, "legs cancel → no FX line");
    assert!(
        r.is_empty(),
        "is_empty keys off the FX line (nothing net to post)"
    );
    assert_functional_balances(&r);
}

#[test]
fn empty_positions_yield_empty_result() {
    let r = remeasure(&[]).unwrap();
    assert!(r.grain_lines.is_empty());
    assert_eq!(r.fx_unrealized, None);
    assert!(r.is_empty());
}

#[test]
fn negative_carried_is_rejected() {
    let positions = [pos(Side::Debit, -1, 100)];
    assert_eq!(
        remeasure(&positions),
        Err(RevaluationError::NegativeCarriedFunctional)
    );
}

#[test]
fn negative_remeasured_is_rejected() {
    let positions = [pos(Side::Debit, 100, -1)];
    assert_eq!(
        remeasure(&positions),
        Err(RevaluationError::NegativeRemeasured)
    );
}

#[test]
fn scope_normal_side_and_token() {
    assert_eq!(RevaluationScope::Ar.normal_side(), Side::Debit);
    assert_eq!(RevaluationScope::Unallocated.normal_side(), Side::Credit);
    assert_eq!(RevaluationScope::ReusableCredit.normal_side(), Side::Credit);
    assert_eq!(RevaluationScope::Ar.as_token(), "AR");
    assert_eq!(RevaluationScope::Unallocated.as_token(), "UNALLOCATED");
    assert_eq!(
        RevaluationScope::ReusableCredit.as_token(),
        "REUSABLE_CREDIT"
    );
    assert_eq!(
        RevaluationScope::all(),
        [
            RevaluationScope::Ar,
            RevaluationScope::Unallocated,
            RevaluationScope::ReusableCredit
        ]
    );
}
