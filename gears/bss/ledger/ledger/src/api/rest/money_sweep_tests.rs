//! Generated sweeps over the wire money codec (deterministic, no extra test
//! dependency): every scale 0..=28 with coefficients up to the 28-digit bound,
//! both signs, round-trips `PostedMoney -> MoneyDto -> PostedMoney` exactly, and
//! arbitrary amount strings never panic the parser.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::MoneyDto;
use bss_ledger_sdk::{CurrencySpec, PostedMoney};
use rust_decimal::Decimal;

/// A small deterministic generator (64-bit LCG) so failures reproduce exactly.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 11
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A random coefficient of exactly `digits` digits (1..=28).
fn coefficient(rng: &mut Lcg, digits: u32) -> i128 {
    let mut value: i128 = i128::from(1 + rng.below(9));
    for _ in 1..digits {
        value = value * 10 + i128::from(rng.below(10));
    }
    value
}

#[test]
fn generated_postings_round_trip_through_the_wire_dto() {
    let mut rng = Lcg(0x5eed_0001);
    let mut checked = 0_u32;
    for scale in 0..=28_u8 {
        let spec = CurrencySpec::try_new("XTS".to_owned(), scale).unwrap();
        for digits in 1..=28_u32 {
            for negative in [false, true] {
                let mut mantissa = coefficient(&mut rng, digits);
                if negative {
                    mantissa = -mantissa;
                }
                // Any fractional placement up to the currency scale is a valid
                // posting increment; the coefficient bound is 28 digits.
                let places = u32::try_from(rng.below(u64::from(scale) + 1)).unwrap();
                let amount = Decimal::from_i128_with_scale(mantissa, places);
                let posted = PostedMoney::try_new(amount, spec.clone()).unwrap();
                let wire = MoneyDto::from(&posted);
                assert_eq!(wire.currency_scale, scale);
                let back = PostedMoney::try_from(wire.clone()).unwrap();
                assert_eq!(back, posted, "{wire:?}");
                // The wire text is canonical: re-emitting it is a fixed point.
                assert_eq!(MoneyDto::from(&back), wire);
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 29 * 28 * 2);
}

#[test]
fn arbitrary_amount_text_never_panics_the_parser() {
    const ALPHABET: &[u8] = b"0123456789.-+eE _x\xc3\xa9";
    let mut rng = Lcg(0x5eed_0002);
    for _ in 0..20_000 {
        let len = usize::try_from(rng.below(70)).unwrap();
        let bytes: Vec<u8> = (0..len)
            .map(|_| {
                ALPHABET
                    [usize::try_from(rng.below(u64::try_from(ALPHABET.len()).unwrap())).unwrap()]
            })
            .collect();
        let amount = String::from_utf8_lossy(&bytes).into_owned();
        let scale = u8::try_from(rng.below(31)).unwrap();
        let dto = MoneyDto {
            amount: amount.clone(),
            currency: "USD".to_owned(),
            currency_scale: scale,
        };
        // Ok or a typed error; anything accepted round-trips exactly.
        if let Ok(posted) = PostedMoney::try_from(dto) {
            assert_eq!(
                PostedMoney::try_from(MoneyDto::from(&posted)).unwrap(),
                posted
            );
        }
    }
}
