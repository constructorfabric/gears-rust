//! Generated round trips for the canonical money text every money column stores.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use bss_ledger_sdk::{CurrencySpec, PostedMoney};
use rust_decimal::Decimal;

use super::{decode_amount, decode_rate, encode_amount};
use crate::domain::model::RepoError;

/// `SplitMix64`: deterministic, so a failing case reproduces exactly.
struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

/// A posting with a 1..=28-digit coefficient, either sign, at a scale 0..=28
/// that admits it.
fn generated_money(rng: &mut SplitMix) -> PostedMoney {
    let digits = u32::try_from(rng.below(28)).unwrap() + 1;
    let wide = (i128::from(rng.next()) << 64) | i128::from(rng.next());
    let mut coefficient = wide.rem_euclid(10_i128.pow(digits));
    if rng.next().is_multiple_of(2) {
        coefficient = -coefficient;
    }
    let amount_scale = u32::try_from(rng.below(29)).unwrap();
    let amount = Decimal::from_i128_with_scale(coefficient, amount_scale).normalize();
    let spare = 28 - amount.scale();
    let scale = amount.scale() + u32::try_from(rng.below(u64::from(spare) + 1)).unwrap();
    let currency = CurrencySpec::try_new("X9".to_owned(), u8::try_from(scale).unwrap()).unwrap();
    PostedMoney::try_new(amount, currency).unwrap()
}

#[test]
fn encode_then_decode_is_identity_for_generated_money() {
    let mut rng = SplitMix(0x001E_D6E5);
    let mut negatives = 0;
    for _ in 0..20_000 {
        let money = generated_money(&mut rng);
        if money.amount().is_sign_negative() {
            negatives += 1;
        }
        let text = encode_amount(&money);
        assert_eq!(
            decode_amount(&text, money.currency().clone()).unwrap(),
            money,
            "{text}"
        );
    }
    assert!(negatives > 5_000, "the sweep must cover negative balances");
}

#[test]
fn extreme_values_round_trip_at_both_scale_bounds() {
    for (text, scale) in [
        ("9999999999999999999999999999", 0),
        ("-9999999999999999999999999999", 0),
        ("0.0000000000000000000000000001", 28),
        ("-0.0000000000000000000000000001", 28),
        ("0", 28),
    ] {
        let currency = CurrencySpec::try_new("X9".to_owned(), scale).unwrap();
        let money = decode_amount(text, currency.clone()).unwrap();
        assert_eq!(encode_amount(&money), text);
        assert_eq!(money.currency(), &currency);
    }
}

#[test]
fn arbitrary_text_never_panics_and_only_canonical_text_decodes() {
    const ALPHABET: &[u8] = b"0123456789.-+eE _";
    let currency = CurrencySpec::try_new("X9".to_owned(), 28).unwrap();
    let mut rng = SplitMix(0x0BAD_C0DE);
    for _ in 0..20_000 {
        let len = usize::try_from(rng.below(40)).unwrap();
        let text: String = (0..len)
            .map(|_| char::from(ALPHABET[usize::try_from(rng.below(17)).unwrap()]))
            .collect();
        match decode_amount(&text, currency.clone()) {
            Ok(money) => assert_eq!(encode_amount(&money), text),
            Err(RepoError::InvalidStoredMoney(_)) => {}
            Err(other) => panic!("{text:?}: unexpected {other:?}"),
        }
        if let Ok(rate) = decode_rate(&text) {
            assert!(rate > Decimal::ZERO, "{text:?}");
        }
    }
}

#[test]
fn a_corrupt_rate_names_the_stored_text() {
    for text in ["1.50", "-1", "0", "x"] {
        let Err(RepoError::InvalidStoredMoney(detail)) = decode_rate(text) else {
            panic!("{text:?} must be refused");
        };
        assert!(detail.contains(&format!("{text:?}")), "{detail}");
    }
}
