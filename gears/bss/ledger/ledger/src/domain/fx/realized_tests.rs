//! Unit tests for the pure realized-FX poster (`domain/fx/realized.rs`): the
//! worked-example-C oracle, sign-by-role (loss DR / gain CR), the same-rate
//! no-FX case, WAC pro-rata partial relief, the blended-grain (two-rate) carry,
//! the no-cross-grain-average invariant, and the malformed-leg errors. Every
//! result is also checked against the functional-column balance invariant
//! (relief legs + FX line net to zero).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use bss_ledger_sdk::money::CurrencySpec;
fn money(minor: i64) -> PostedMoney {
    PostedMoney::try_new(
        Decimal::from_i128_with_scale(i128::from(minor), 2),
        CurrencySpec::try_new("USD".into(), 2).unwrap(),
    )
    .unwrap()
}

/// A `ClosingLeg` literal.
fn leg(
    side: Side,
    carried_functional: i64,
    carried_transaction: i64,
    relieved_transaction: i64,
) -> ClosingLeg {
    ClosingLeg {
        side,
        carried_functional: money(carried_functional),
        carried_transaction: money(carried_transaction),
        relieved_transaction: money(relieved_transaction),
    }
}

/// Assert the close entry's functional column balances: Σ DR (relief legs + FX
/// line) == Σ CR. This is the invariant `realize` must guarantee by construction.
fn assert_functional_balances(legs: &[ClosingLeg], r: &RealizedFx) {
    let mut dr = ExactAmount::from_decimal(Decimal::ZERO);
    let mut cr = ExactAmount::from_decimal(Decimal::ZERO);
    for (leg, f) in legs.iter().zip(&r.leg_functional) {
        match leg.side {
            Side::Debit => {
                dr = dr
                    .checked_add(&ExactAmount::from_decimal(f.amount()))
                    .unwrap();
            }
            Side::Credit => {
                cr = cr
                    .checked_add(&ExactAmount::from_decimal(f.amount()))
                    .unwrap();
            }
        }
    }
    if let Some(fx) = &r.fx_line {
        match fx.side {
            Side::Debit => {
                dr = dr
                    .checked_add(&ExactAmount::from_decimal(fx.functional.amount()))
                    .unwrap();
            }
            Side::Credit => {
                cr = cr
                    .checked_add(&ExactAmount::from_decimal(fx.functional.amount()))
                    .unwrap();
            }
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
    assert_eq!(r.leg_functional, vec![money(12_960), money(13_200)]);
    assert_eq!(
        r.fx_line,
        Some(RealizedFxLine {
            side: Side::Debit,
            functional: money(240),
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
            functional: money(240),
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
    assert_eq!(r.leg_functional, vec![money(6_480), money(6_600)]);
    assert_eq!(
        r.fx_line,
        Some(RealizedFxLine {
            side: Side::Debit,
            functional: money(120),
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
        r.leg_functional,
        vec![money(13_080)],
        "relief is the grain's blended WAC, not a per-settlement rate"
    );
    // One unbalanced leg → the whole relief is the realized FX (DR relief short on
    // the CR side → CR FX gain of 13080).
    assert_eq!(
        r.fx_line,
        Some(RealizedFxLine {
            side: Side::Credit,
            functional: money(13_080),
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
            r.leg_functional,
            vec![money(cf)],
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
    assert_eq!(r.leg_functional[0], money(13_200), "AR keeps its own carry");
    assert_eq!(
        r.leg_functional[1],
        money(12_960),
        "Unallocated keeps its own carry (not (13200+12960)/2 = 13080)"
    );
}

#[test]
fn empty_close_is_a_no_op() {
    let r = realize(&[]).unwrap();
    assert!(r.leg_functional.is_empty());
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

#[test]
fn wac_metadata_must_match_even_on_zero() {
    let wrong_scale = PostedMoney::try_new(
        Decimal::ZERO,
        CurrencySpec::try_new("USD".into(), 3).unwrap(),
    )
    .unwrap();
    assert_eq!(
        carried_relief(&money(0), &money(100), &wrong_scale),
        Err(RealizedFxError::Exact(ExactError::Money(
            bss_ledger_sdk::money::MoneyError::ScaleMismatch
        )))
    );
    let mut legs = [
        leg(Side::Debit, 0, 100, 100),
        leg(Side::Credit, 0, 100, 100),
    ];
    legs[1].carried_functional = PostedMoney::try_new(
        Decimal::ZERO,
        CurrencySpec::try_new("EUR".into(), 2).unwrap(),
    )
    .unwrap();
    assert_eq!(
        realize(&legs),
        Err(RealizedFxError::Exact(ExactError::Money(
            bss_ledger_sdk::money::MoneyError::CurrencyMismatch
        )))
    );
    legs[1].carried_functional = wrong_scale;
    assert_eq!(
        realize(&legs),
        Err(RealizedFxError::Exact(ExactError::Money(
            bss_ledger_sdk::money::MoneyError::ScaleMismatch
        )))
    );
}

#[test]
fn wide_independent_full_close_cancels_and_final_net_overflow_rejects() {
    let huge = PostedMoney::try_new(
        Decimal::from_str_exact("9999999999999999999999999999").unwrap(),
        CurrencySpec::try_new("USD".into(), 0).unwrap(),
    )
    .unwrap();
    let make = |side| ClosingLeg {
        side,
        carried_functional: huge.clone(),
        carried_transaction: money(7),
        relieved_transaction: money(7),
    };
    let legs = [
        make(Side::Debit),
        make(Side::Debit),
        make(Side::Credit),
        make(Side::Credit),
    ];
    let r = realize(&legs).unwrap();
    assert_eq!(r.leg_functional, vec![huge.clone(); 4]);
    assert_eq!(r.fx_line, None);
    assert_functional_balances(&legs, &r);
    assert_eq!(
        realize(&legs[..2]),
        Err(RealizedFxError::Exact(ExactError::Money(
            bss_ledger_sdk::money::MoneyError::AmountOutOfRange
        )))
    );
}

#[test]
fn exact_wac_nonterminating_ratio_and_half_even() {
    assert_eq!(
        carried_relief(&money(100), &money(3), &money(1)).unwrap(),
        money(33)
    );
    assert_eq!(
        carried_relief(&money(1), &money(2), &money(1)).unwrap(),
        money(0)
    );
    assert_eq!(
        carried_relief(&money(3), &money(2), &money(1)).unwrap(),
        money(2)
    );
}
