//! The partial-return fee share rounds HALF_EVEN, not down.
#![allow(clippy::unwrap_used)]

use bss_ledger_sdk::{CurrencySpec, PostedMoney, canonical_decimal, parse_decimal};

use super::pro_rata_fee_share;

fn eur(text: &str) -> PostedMoney {
    PostedMoney::try_new(
        parse_decimal(text).unwrap(),
        CurrencySpec::try_new("EUR".into(), 2).unwrap(),
    )
    .unwrap()
}

fn share(fee: &str, amount: &str, settled: &str) -> String {
    canonical_decimal(
        pro_rata_fee_share(&eur(fee), &eur(amount), &eur(settled))
            .unwrap()
            .amount(),
    )
}

#[test]
fn partial_return_fee_share_rounds_half_even() {
    // Tie 0.015 rounds to the even 0.02 (floor division gave 0.01), leaving a
    // fee of 0.01 for the second half.
    assert_eq!(share("0.03", "0.5", "1"), "0.02");
    // Tie 0.025 rounds to the even 0.02 (here HALF_EVEN and floor agree).
    assert_eq!(share("0.05", "0.5", "1"), "0.02");
    // Non-ties round to the nearest increment either way.
    assert_eq!(share("0.03", "0.4", "1"), "0.01");
    assert_eq!(share("0.03", "0.6", "1"), "0.02");
    // A full return takes the whole fee.
    assert_eq!(share("0.03", "1", "1"), "0.03");
}
