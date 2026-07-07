//! Unit tests for the pure realized-FX poster (`domain/fx/realized.rs`): the
//! worked-example-C oracle, sign-by-role (loss DR / gain CR), the same-rate
//! no-FX case, WAC pro-rata partial relief, the blended-grain (two-rate) carry,
//! the no-cross-grain-average invariant, and the malformed-leg errors. Every
//! result is also checked against the functional-column balance invariant
//! (relief legs + FX line net to zero).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

/// A `ClosingLeg` literal.
fn leg(
    side: Side,
    carried_functional_minor: i64,
    carried_transaction_minor: i64,
    relieved_transaction_minor: i64,
) -> ClosingLeg {
    ClosingLeg {
        side,
        carried_functional_minor,
        carried_transaction_minor,
        relieved_transaction_minor,
    }
}

/// Assert the close entry's functional column balances: Σ DR (relief legs + FX
/// line) == Σ CR. This is the invariant `realize` must guarantee by construction.
fn assert_functional_balances(legs: &[ClosingLeg], r: &RealizedFx) {
    let mut dr: i128 = 0;
    let mut cr: i128 = 0;
    for (leg, &f) in legs.iter().zip(&r.leg_functional_minor) {
        match leg.side {
            Side::Debit => dr += i128::from(f),
            Side::Credit => cr += i128::from(f),
        }
    }
    if let Some(fx) = r.fx_line {
        match fx.side {
            Side::Debit => dr += i128::from(fx.functional_minor),
            Side::Credit => cr += i128::from(fx.functional_minor),
        }
    }
    assert_eq!(dr, cr, "functional column must balance (DR == CR)");
}

#[test]
fn example_c_full_close_nets_240_usd_loss() {
    // Spec worked example C: USD functional, EUR invoice. Allocate closes both:
    //   DR Unallocated 129.60 (carried) / CR AR 132.00 (carried), 120 EUR each.
    // Functional short 2.40 on the DR side → DR FX loss 2.40 (240 minor).
    let legs = [
        leg(Side::Debit, 12_960, 12_000, 12_000), // Unallocated, carried 129.60
        leg(Side::Credit, 13_200, 12_000, 12_000), // AR, carried 132.00
    ];
    let r = realize(&legs).unwrap();
    // Full close → each leg relieves its whole carried functional.
    assert_eq!(r.leg_functional_minor, vec![12_960, 13_200]);
    assert_eq!(
        r.fx_line,
        Some(RealizedFxLine {
            side: Side::Debit,
            functional_minor: 240,
        }),
        "net 2.40 USD realized LOSS on the DR side"
    );
    assert_functional_balances(&legs, &r);
}

#[test]
fn gain_direction_credits_short_emits_credit_fx() {
    // Mirror of example C with the carried values swapped → credits short → a
    // realized GAIN on the CR side.
    let legs = [
        leg(Side::Debit, 13_200, 12_000, 12_000),
        leg(Side::Credit, 12_960, 12_000, 12_000),
    ];
    let r = realize(&legs).unwrap();
    assert_eq!(
        r.fx_line,
        Some(RealizedFxLine {
            side: Side::Credit,
            functional_minor: 240,
        }),
        "credits short → realized GAIN on the CR side"
    );
    assert_functional_balances(&legs, &r);
}

#[test]
fn same_rate_close_emits_no_fx_line() {
    // Both legs carried at the same rate → the relief nets to zero → no realized FX.
    let legs = [
        leg(Side::Debit, 12_000, 12_000, 12_000),
        leg(Side::Credit, 12_000, 12_000, 12_000),
    ];
    let r = realize(&legs).unwrap();
    assert_eq!(r.fx_line, None, "a same-rate close posts no FX line");
    assert_functional_balances(&legs, &r);
}

#[test]
fn partial_close_relieves_wac_prorata_half() {
    // Relieve HALF of example C's position → half the relief + half the FX (1.20).
    let legs = [
        leg(Side::Debit, 12_960, 12_000, 6_000), // 12960 * 6000/12000 = 6480
        leg(Side::Credit, 13_200, 12_000, 6_000), // 13200 * 6000/12000 = 6600
    ];
    let r = realize(&legs).unwrap();
    assert_eq!(r.leg_functional_minor, vec![6_480, 6_600]);
    assert_eq!(
        r.fx_line,
        Some(RealizedFxLine {
            side: Side::Debit,
            functional_minor: 120,
        }),
        "half close → half the realized loss (1.20)"
    );
    assert_functional_balances(&legs, &r);
}

#[test]
fn blended_grain_two_rates_relieves_at_wac() {
    // A grain that took two settlements at different rates (132.00 + 129.60 over
    // 24000 EUR) carries the BLEND (261.60 / 24000 = WAC 1.09). Relieving 12000
    // relieves 130.80 (13080 minor) — the WAC, not either original rate.
    let legs = [leg(Side::Debit, 26_160, 24_000, 12_000)];
    let r = realize(&legs).unwrap();
    assert_eq!(
        r.leg_functional_minor,
        vec![13_080],
        "relief is the grain's blended WAC, not a per-settlement rate"
    );
    // One unbalanced leg → the whole relief is the realized FX (DR relief short on
    // the CR side → CR FX gain of 13080).
    assert_eq!(
        r.fx_line,
        Some(RealizedFxLine {
            side: Side::Credit,
            functional_minor: 13_080,
        })
    );
    assert_functional_balances(&legs, &r);
}

#[test]
fn full_close_relieves_exact_carried_no_drift() {
    // relieved == carried_transaction → relieved functional == carried functional
    // exactly (f * t / t = f), regardless of the carried values.
    for (cf, ct) in [(13_201, 12_000), (1, 7), (999_983, 100_001)] {
        let l = leg(Side::Debit, cf, ct, ct);
        let r = realize(std::slice::from_ref(&l)).unwrap();
        assert_eq!(
            r.leg_functional_minor,
            vec![cf],
            "a full close relieves the exact carried functional ({cf}/{ct})"
        );
    }
}

#[test]
fn each_leg_uses_its_own_carried_no_cross_grain_average() {
    // Two grains carried at DIFFERENT rates closed in one entry: each leg relieves
    // ITS OWN carried functional — NEVER a cross-grain average (spec §3.5).
    let legs = [
        leg(Side::Credit, 13_200, 12_000, 12_000), // AR @1.10
        leg(Side::Debit, 12_960, 12_000, 12_000),  // Unallocated @1.08
    ];
    let r = realize(&legs).unwrap();
    assert_eq!(r.leg_functional_minor[0], 13_200, "AR keeps its own carry");
    assert_eq!(
        r.leg_functional_minor[1], 12_960,
        "Unallocated keeps its own carry (not (13200+12960)/2 = 13080)"
    );
}

#[test]
fn empty_close_is_a_no_op() {
    let r = realize(&[]).unwrap();
    assert!(r.leg_functional_minor.is_empty());
    assert_eq!(r.fx_line, None);
}

#[test]
fn non_positive_carried_transaction_rejected() {
    assert_eq!(
        realize(&[leg(Side::Debit, 100, 0, 0)]),
        Err(RealizedFxError::NonPositiveCarriedTransaction)
    );
    assert_eq!(
        realize(&[leg(Side::Debit, 100, -5, 1)]),
        Err(RealizedFxError::NonPositiveCarriedTransaction)
    );
}

#[test]
fn relieved_out_of_range_rejected() {
    // relieved > carried_transaction.
    assert_eq!(
        realize(&[leg(Side::Debit, 100, 12_000, 12_001)]),
        Err(RealizedFxError::RelievedOutOfRange)
    );
    // relieved == 0 (nothing relieved is not a close leg).
    assert_eq!(
        realize(&[leg(Side::Debit, 100, 12_000, 0)]),
        Err(RealizedFxError::RelievedOutOfRange)
    );
}

#[test]
fn negative_carried_functional_rejected() {
    assert_eq!(
        realize(&[leg(Side::Debit, -1, 12_000, 12_000)]),
        Err(RealizedFxError::NegativeCarriedFunctional)
    );
}
