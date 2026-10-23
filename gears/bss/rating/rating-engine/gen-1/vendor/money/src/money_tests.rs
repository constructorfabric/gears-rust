//! Validated money contract tests.
#![allow(clippy::unwrap_used)]

use super::{CurrencySpec, MoneyError, PostedMoney, canonical_decimal, parse_decimal};

#[test]
fn eur_posting_requires_whole_cents() {
    let eur = CurrencySpec::try_new("EUR".into(), 2).unwrap();
    let good = PostedMoney::try_new(parse_decimal("12.340").unwrap(), eur.clone()).unwrap();
    assert_eq!(canonical_decimal(good.amount()), "12.34");
    assert_eq!(
        PostedMoney::try_new(parse_decimal("0.047").unwrap(), eur),
        Err(MoneyError::InvalidPostingIncrement),
    );
}

#[test]
fn currency_shape_and_scale_are_validated_without_a_registry() {
    for code in ["EUR", "X9", "123", "ABCDEFGHIJKLMNOP"] {
        let currency = CurrencySpec::try_new(code.into(), 28).unwrap();
        assert_eq!(currency.code(), code);
        assert_eq!(currency.scale(), 28);
        // The same code at another scale is another currency spec.
        assert_ne!(currency, CurrencySpec::try_new(code.into(), 2).unwrap());
    }
    for code in ["", "eur", "EUR_", "EU R", "\u{c9}UR", "ABCDEFGHIJKLMNOPQ"] {
        assert_eq!(
            CurrencySpec::try_new(code.into(), 2),
            Err(MoneyError::InvalidCurrency)
        );
    }
    assert!(CurrencySpec::try_new("EUR".into(), 3).is_ok());
    assert_eq!(
        CurrencySpec::try_new("EUR".into(), 29),
        Err(MoneyError::ScaleOutOfRange)
    );
}

#[test]
fn supported_posting_scales_preserve_both_signs_and_zero() {
    for (scale, smallest) in [
        (0, "1"),
        (2, "0.01"),
        (3, "0.001"),
        (8, "0.00000001"),
        (28, "0.0000000000000000000000000001"),
    ] {
        let currency = CurrencySpec::try_new("X9".into(), scale).unwrap();
        for text in [smallest.to_owned(), format!("-{smallest}"), "0".to_owned()] {
            let money =
                PostedMoney::try_new(parse_decimal(&text).unwrap(), currency.clone()).unwrap();
            assert_eq!(canonical_decimal(money.amount()), text);
            assert_eq!(money.currency(), &currency);
            // The ledger relies on this: the same amount and code at a
            // different stored scale is a different posting.
            if scale < 28 {
                let wider = CurrencySpec::try_new("X9".into(), scale + 1).unwrap();
                assert_ne!(PostedMoney::try_new(money.amount(), wider).unwrap(), money);
            }
        }
    }
}

#[test]
fn postings_reject_fractional_increments_without_rounding() {
    for (scale, text) in [(0, "0.1"), (2, "0.001"), (3, "0.0001"), (8, "0.000000001")] {
        let currency = CurrencySpec::try_new("X9".into(), scale).unwrap();
        for amount in [text.to_owned(), format!("-{text}")] {
            assert_eq!(
                PostedMoney::try_new(parse_decimal(&amount).unwrap(), currency.clone()),
                Err(MoneyError::InvalidPostingIncrement)
            );
        }
    }
}

#[test]
fn normalized_coefficients_are_limited_to_28_digits() {
    for text in [
        "9999999999999999999999999999",
        "-9999999999999999999999999999",
        "0.9999999999999999999999999999",
        "-0.9999999999999999999999999999",
    ] {
        assert_eq!(canonical_decimal(parse_decimal(text).unwrap()), text);
    }
    for text in [
        "10000000000000000000000000000",
        "-10000000000000000000000000000",
        "1.0000000000000000000000000001",
        "-1.0000000000000000000000000001",
    ] {
        assert_eq!(
            parse_decimal(text),
            Err(MoneyError::AmountOutOfRange),
            "{text}"
        );
    }
}

#[test]
fn decimal_callers_cannot_bypass_the_posting_amount_limit() {
    let currency = CurrencySpec::try_new("X9".into(), 28).unwrap();
    for text in [
        "10000000000000000000000000000",
        "-10000000000000000000000000000",
        "1.0000000000000000000000000001",
    ] {
        let amount = rust_decimal::Decimal::from_str_exact(text).unwrap();
        assert_eq!(
            PostedMoney::try_new(amount, currency.clone()),
            Err(MoneyError::AmountOutOfRange)
        );
    }
}

#[test]
fn decimal_syntax_rejects_non_plain_forms_and_negative_zero() {
    for text in [
        "", "-", "+1", " 1", "1 ", "1\n", "1e2", "1E-2", "01", "00.1", "-01", ".1", "1.", "1.2.3",
        "1_000", "1,000", "--1", "\u{ff11}", "-0", "-0.0", "-0.000",
    ] {
        assert_eq!(
            parse_decimal(text),
            Err(MoneyError::InvalidDecimal),
            "{text:?}"
        );
    }
}

#[test]
fn exact_parser_rejects_values_the_carrier_would_round() {
    let text = "0.12345678901234567890123456789";
    assert!(text.parse::<rust_decimal::Decimal>().is_ok());
    assert_eq!(parse_decimal(text), Err(MoneyError::InvalidDecimal));
    assert_eq!(
        parse_decimal("0.00000000000000000000000000001"),
        Err(MoneyError::InvalidDecimal)
    );
}

#[test]
fn input_byte_limit_precedes_normalization() {
    let at_limit = format!("1.{}", "0".repeat(62));
    assert_eq!(at_limit.len(), 64);
    assert_eq!(canonical_decimal(parse_decimal(&at_limit).unwrap()), "1");
    let too_long = format!("{at_limit}0");
    assert_eq!(parse_decimal(&too_long), Err(MoneyError::InvalidDecimal));
}

#[test]
fn canonical_text_normalizes_equivalent_values() {
    for (text, canonical) in [
        ("0", "0"),
        ("0.000", "0"),
        ("10.000", "10"),
        ("10.0", "10"),
        ("10", "10"),
        ("-12.3400", "-12.34"),
        (
            "9999999999999999999999999999.000",
            "9999999999999999999999999999",
        ),
    ] {
        let amount = parse_decimal(text).unwrap();
        assert_eq!(canonical_decimal(amount), canonical);
        assert_eq!(parse_decimal(&canonical_decimal(amount)).unwrap(), amount);
    }
    let negative_zero = rust_decimal::Decimal::from_str_exact("-0.00").unwrap();
    assert_eq!(canonical_decimal(negative_zero), "0");
    let currency = CurrencySpec::try_new("EUR".into(), 2).unwrap();
    assert_eq!(
        canonical_decimal(
            PostedMoney::try_new(negative_zero, currency)
                .unwrap()
                .amount()
        ),
        "0"
    );
}

#[test]
fn coefficient_overflow_is_out_of_range_and_excess_precision_is_invalid() {
    // A coefficient beyond 28 digits is out of range, also past the carrier.
    for text in [
        "10000000000000000000000000000",
        "100000000000000000000000000000",
        "999999999999999999999999999.99",
    ] {
        assert_eq!(
            parse_decimal(text),
            Err(MoneyError::AmountOutOfRange),
            "{text}"
        );
    }
    // More than 28 fractional places cannot be held exactly at all.
    assert_eq!(
        parse_decimal("0.00000000000000000000000000001"),
        Err(MoneyError::InvalidDecimal)
    );
}

#[test]
fn money_displays_as_canonical_text_and_code() {
    let eur = CurrencySpec::try_new("EUR".into(), 2).unwrap();
    let money = PostedMoney::try_new(parse_decimal("12.340").unwrap(), eur).unwrap();
    assert_eq!(money.to_string(), "12.34 EUR");
}

/// A deterministic 64-bit generator (`SplitMix64`), so the round-trip sweep below
/// covers many in-contract values without a property-test dependency and
/// reproduces any failure exactly.
struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

#[test]
fn canonical_text_round_trips_for_generated_in_contract_decimals() {
    let limit: i128 = 10_i128.pow(28);
    let mut rng = SplitMix(0x5EED);
    for _ in 0..20_000 {
        // A coefficient of 1..=28 digits, either sign, at scale 0..=28.
        let digits = u32::try_from(rng.next() % 28).unwrap() + 1;
        let wide = (i128::from(rng.next()) << 64) | i128::from(rng.next());
        let mut coefficient = wide.rem_euclid(10_i128.pow(digits));
        if rng.next().is_multiple_of(2) {
            coefficient = -coefficient;
        }
        assert!(coefficient.abs() < limit);
        let scale = u32::try_from(rng.next() % 29).unwrap();
        let value = rust_decimal::Decimal::from_i128_with_scale(coefficient, scale);
        let text = canonical_decimal(value);
        assert_eq!(parse_decimal(&text), Ok(value.normalize()), "{text}");
        assert_eq!(canonical_decimal(parse_decimal(&text).unwrap()), text);
        // Money at its own scale survives the same round trip.
        let spec = CurrencySpec::try_new("X9".into(), u8::try_from(scale).unwrap()).unwrap();
        let money = PostedMoney::try_new(value, spec.clone()).unwrap();
        assert_eq!(
            PostedMoney::try_new(
                parse_decimal(&money.to_string().replace(" X9", "")).unwrap(),
                spec
            )
            .unwrap(),
            money
        );
    }
}

#[test]
fn parse_decimal_never_panics_on_generated_text() {
    const ALPHABET: &[u8] = b"0123456789.-+eE _x\x00";
    let mut rng = SplitMix(0xFACE);
    for _ in 0..20_000 {
        let len = usize::try_from(rng.next() % 70).unwrap();
        let text: String = (0..len)
            .map(|_| {
                let index = usize::try_from(rng.next()).unwrap() % ALPHABET.len();
                char::from(ALPHABET[index])
            })
            .collect();
        // Any outcome is allowed except a panic; an accepted value is canonical
        // after one more round.
        if let Ok(value) = parse_decimal(&text) {
            assert_eq!(
                parse_decimal(&canonical_decimal(value)),
                Ok(value),
                "{text}"
            );
        }
    }
}
