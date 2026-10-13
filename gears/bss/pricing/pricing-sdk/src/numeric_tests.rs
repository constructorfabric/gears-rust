//! Pricing numeric limits: text rules, bounds and the typed validators.
#![allow(clippy::unwrap_used)]

use bss_money::CurrencySpec;
use rust_decimal::Decimal;

use super::{
    NumericError, QUANTITY_FRACTIONAL_DIGITS, RATE_FRACTIONAL_DIGITS, parse_positive_quantity,
    parse_rate, parse_threshold, validate_minimum_fee, validate_positive_quantity, validate_rate,
    validate_threshold,
};

type Parser = fn(&str) -> Result<Decimal, NumericError>;
type Validator = fn(Decimal) -> Result<Decimal, NumericError>;

const PARSERS: [Parser; 3] = [parse_rate, parse_threshold, parse_positive_quantity];

/// Text that `Decimal::from_str` accepts today but the wire format must refuse.
const SYNTAX_REJECTS: [&str; 8] = ["+1", "1_000", "01", "-0", "1e-3", "1.", ".1", " 1"];

fn dec(text: &str) -> Decimal {
    Decimal::from_str_exact(text).unwrap()
}

fn eur() -> CurrencySpec {
    CurrencySpec::try_new("EUR".into(), 2).unwrap()
}

/// `0.` followed by `places - 1` zeros and a one: the smallest value with that many places.
fn smallest_with_places(places: usize) -> String {
    format!("0.{}1", "0".repeat(places - 1))
}

#[test]
fn the_limits_are_the_agreed_ones() {
    assert_eq!(RATE_FRACTIONAL_DIGITS, 12);
    assert_eq!(QUANTITY_FRACTIONAL_DIGITS, 28);
}

#[test]
fn every_parser_rejects_text_outside_the_plain_decimal_format() {
    for parse in PARSERS {
        for text in SYNTAX_REJECTS {
            assert_eq!(
                parse(text),
                Err(NumericError::InvalidText),
                "a parser must refuse the syntax reject vectors",
            );
        }
        for text in [
            "", "-", ".", "1.2.3", "1,5", "abc", "1 ", "0x10", "1E3", "\u{663}",
        ] {
            assert_eq!(
                parse(text),
                Err(NumericError::InvalidText),
                "a parser must refuse text that is not a decimal",
            );
        }
    }
}

#[test]
fn every_parser_refuses_a_negative_value() {
    for parse in PARSERS {
        assert_eq!(parse("-1"), Err(NumericError::Negative));
        assert_eq!(parse("-0.5"), Err(NumericError::Negative));
    }
}

#[test]
fn parsers_accept_plain_decimals_and_return_the_normalized_value() {
    for parse in PARSERS {
        assert_eq!(parse("0.047").unwrap().to_string(), "0.047");
        assert_eq!(parse("0.10").unwrap().to_string(), "0.1");
        assert_eq!(parse("12.340").unwrap().to_string(), "12.34");
        assert_eq!(parse("7").unwrap().to_string(), "7");
        assert_eq!(parse("7.000").unwrap().to_string(), "7");
    }
}

#[test]
fn rate_and_threshold_accept_zero_but_a_quantity_does_not() {
    for text in ["0", "0.0", "0.000"] {
        let rate = parse_rate(text).unwrap();
        assert!(rate.is_zero());
        assert_eq!(rate.scale(), 0);
        assert!(parse_threshold(text).unwrap().is_zero());
        assert_eq!(
            parse_positive_quantity(text),
            Err(NumericError::NotPositive)
        );
    }
}

#[test]
fn a_29th_fractional_digit_is_out_of_range_for_every_parser() {
    let text = smallest_with_places(29);
    assert_eq!(text, "0.00000000000000000000000000001");
    for parse in PARSERS {
        assert_eq!(parse(&text), Err(NumericError::OutOfRange));
        assert_eq!(
            parse("1.00000000000000000000000000019"),
            Err(NumericError::OutOfRange)
        );
        assert_eq!(
            parse("123456789.12345678901234567890123"),
            Err(NumericError::OutOfRange)
        );
    }
}

#[test]
fn trailing_zeros_past_the_28th_place_are_harmless() {
    for parse in [parse_threshold, parse_positive_quantity] {
        assert_eq!(
            parse("0.0000000000000000000000000001000").unwrap().scale(),
            28
        );
        assert_eq!(parse("1.00000000000000000000000000000").unwrap().scale(), 0);
    }
}

#[test]
fn a_negative_value_with_a_29th_place_is_still_out_of_range_not_invalid_text() {
    for parse in PARSERS {
        assert_eq!(
            parse("-0.00000000000000000000000000001"),
            Err(NumericError::OutOfRange)
        );
    }
}

#[test]
fn a_29th_place_is_only_out_of_range_when_the_rest_is_valid_text() {
    for parse in PARSERS {
        assert_eq!(
            parse("01.00000000000000000000000000001"),
            Err(NumericError::InvalidText)
        );
        assert_eq!(
            parse("1.0000000000000000000000000000e1"),
            Err(NumericError::InvalidText)
        );
        assert_eq!(
            parse("+0.00000000000000000000000000001"),
            Err(NumericError::InvalidText)
        );
        assert_eq!(
            parse("0.0000000000000000000000000000\u{663}1"),
            Err(NumericError::InvalidText)
        );
    }
}

#[test]
fn a_28_digit_coefficient_passes_and_the_next_integer_does_not() {
    let largest = "9999999999999999999999999999";
    let next = "10000000000000000000000000000";
    for parse in [parse_rate, parse_threshold, parse_positive_quantity] {
        assert_eq!(parse(largest).unwrap().to_string(), largest);
        assert_eq!(parse(next), Err(NumericError::OutOfRange));
        // More than a 96-bit carrier can hold is the same refusal, not a syntax error.
        assert_eq!(
            parse("99999999999999999999999999999"),
            Err(NumericError::OutOfRange)
        );
    }
}

#[test]
fn the_coefficient_limit_counts_fractional_digits_too() {
    // 17 integer digits and 12 places: 29 digits.
    assert_eq!(
        parse_rate("99999999999999999.999999999999"),
        Err(NumericError::OutOfRange)
    );
    assert_eq!(
        parse_threshold("999999999999999.9999999999999"),
        Ok(dec("999999999999999.9999999999999"))
    );
    assert_eq!(
        parse_threshold("9999999999999999.9999999999999"),
        Err(NumericError::OutOfRange)
    );
    assert_eq!(
        parse_rate("9999999999999999.999999999999")
            .unwrap()
            .to_string(),
        "9999999999999999.999999999999"
    );
}

#[test]
fn a_rate_may_have_twelve_places_and_not_thirteen() {
    assert_eq!(
        parse_rate(&smallest_with_places(12)).unwrap().scale(),
        RATE_FRACTIONAL_DIGITS
    );
    assert_eq!(
        parse_rate(&smallest_with_places(13)),
        Err(NumericError::OutOfRange)
    );
    // Trailing zeros are not places: this is 12 places once normalized.
    assert_eq!(
        parse_rate("1.0000000000010").unwrap().to_string(),
        "1.000000000001"
    );
    assert_eq!(parse_rate("1.0000000000001"), Err(NumericError::OutOfRange));
}

#[test]
fn a_threshold_and_a_quantity_may_have_the_smallest_scale_28_value() {
    let smallest = smallest_with_places(28);
    for parse in [parse_threshold, parse_positive_quantity] {
        let value = parse(&smallest).unwrap();
        assert_eq!(value.scale(), QUANTITY_FRACTIONAL_DIGITS);
        assert_eq!(value.mantissa(), 1);
    }
    assert_eq!(parse_rate(&smallest), Err(NumericError::OutOfRange));
}

#[test]
fn the_text_limit_is_64_bytes() {
    // 64 bytes: `1.` and 62 zeros, which is the value 1.
    let at_limit = format!("1.{}", "0".repeat(62));
    let over_limit = format!("1.{}", "0".repeat(63));
    assert_eq!(at_limit.len(), 64);
    assert_eq!(over_limit.len(), 65);
    for parse in PARSERS {
        assert_eq!(parse(&at_limit).unwrap().to_string(), "1");
        assert_eq!(parse(&over_limit), Err(NumericError::InvalidText));
    }
    // 64 and 65 integer digits are both too wide; only the longer one is also too long.
    let digits_64 = format!("1{}", "0".repeat(63));
    let digits_65 = format!("1{}", "0".repeat(64));
    for parse in PARSERS {
        assert_eq!(parse(&digits_64), Err(NumericError::OutOfRange));
        assert_eq!(parse(&digits_65), Err(NumericError::InvalidText));
    }
}

#[test]
fn too_many_places_stay_out_of_range_in_text_over_64_bytes() {
    // 70 bytes, 68 places: the text is long, but its only fault is the places.
    let long = format!("0.{}1", "0".repeat(67));
    assert_eq!(long.len(), 70);
    for parse in PARSERS {
        assert_eq!(parse(&long), Err(NumericError::OutOfRange));
    }
}

/// The parsers over a broad table of heads, in place of a generated set: a value reads back from
/// its canonical text as the same value, and that text reads back as itself. Valid text carried
/// past the 28th place, ending in a nonzero digit, is out of range and never invalid text, whatever
/// its sign, its integer part or how far it runs, so the probe behind that answer agrees with the
/// parser for every head; invalid text carried as far stays invalid text.
#[test]
fn the_parsers_round_trip_and_the_excess_places_probe_agrees_over_a_broad_table() {
    let wholes = ["0", "1", "7", "10", "12345", "9999999999999999"];
    let fractions = [
        "",
        "0",
        "5",
        "05",
        "50",
        "123456789012",
        "000000000001",
        "1234567890123456789012345678",
    ];
    for whole in wholes {
        for fraction in fractions {
            let text = if fraction.is_empty() {
                whole.to_owned()
            } else {
                format!("{whole}.{fraction}")
            };
            for parse in PARSERS {
                match parse(&text) {
                    Ok(value) => {
                        let canonical = bss_money::canonical_decimal(value);
                        let again = parse(&canonical).unwrap();
                        assert_eq!(again, value, "a value reads back from its canonical text");
                        assert_eq!(
                            bss_money::canonical_decimal(again),
                            canonical,
                            "canonical text reads back as itself"
                        );
                    }
                    Err(error) => assert!(
                        matches!(error, NumericError::OutOfRange | NumericError::NotPositive),
                        "valid text is refused only for its size or as a zero quantity"
                    ),
                }
            }
            for sign in ["", "-"] {
                for extra in [1, 2, 12, 30] {
                    let padded = format!("{sign}{whole}.{fraction:0<28}{}7", "0".repeat(extra - 1));
                    for parse in PARSERS {
                        assert_eq!(
                            parse(&padded),
                            Err(NumericError::OutOfRange),
                            "valid text past the 28th place is out of range"
                        );
                    }
                }
            }
        }
    }
    let past = format!("{}7", "0".repeat(28));
    for head in ["01", "+1", " 1", "1_0", "0x1", "\u{661}", "-01", ""] {
        for parse in PARSERS {
            assert_eq!(
                parse(&format!("{head}.{past}")),
                Err(NumericError::InvalidText),
                "invalid text past the 28th place stays invalid text"
            );
        }
    }
    for parse in PARSERS {
        assert_eq!(
            parse(&format!("1.{}x{past}", "0".repeat(10))),
            Err(NumericError::InvalidText),
            "a stray character in the fraction stays invalid text"
        );
    }
}

#[test]
fn the_typed_validators_normalize_and_accept_the_same_values() {
    let cases: [(&str, Validator, &str); 7] = [
        ("0.047", validate_rate, "0.047"),
        ("0.10", validate_rate, "0.1"),
        ("12.340", validate_rate, "12.34"),
        ("0.047", validate_threshold, "0.047"),
        ("0.10", validate_threshold, "0.1"),
        ("12.340", validate_positive_quantity, "12.34"),
        ("0.10", validate_positive_quantity, "0.1"),
    ];
    for (input, validate, expected) in cases {
        assert_eq!(validate(dec(input)).unwrap().to_string(), expected);
    }
}

#[test]
fn the_typed_validators_refuse_negative_values() {
    for validate in [
        validate_rate,
        validate_threshold,
        validate_positive_quantity,
    ] {
        assert_eq!(validate(dec("-1")), Err(NumericError::Negative));
        assert_eq!(validate(dec("-0.5")), Err(NumericError::Negative));
    }
}

#[test]
fn the_typed_validators_enforce_the_coefficient_limit() {
    let largest = dec("9999999999999999999999999999");
    let next = Decimal::from_i128_with_scale(10_i128.pow(28), 0);
    for validate in [
        validate_rate,
        validate_threshold,
        validate_positive_quantity,
    ] {
        assert_eq!(validate(largest), Ok(largest));
        assert_eq!(validate(next), Err(NumericError::OutOfRange));
        assert_eq!(validate(-next), Err(NumericError::OutOfRange));
    }
    // The limit is on the normalized value, so a long run of trailing zeros is harmless.
    assert_eq!(
        validate_rate(Decimal::from_i128_with_scale(10_i128.pow(28), 1))
            .unwrap()
            .to_string(),
        "1000000000000000000000000000"
    );
}

#[test]
fn the_typed_rate_validator_allows_twelve_places_and_not_thirteen() {
    assert_eq!(validate_rate(Decimal::new(1, 12)), Ok(Decimal::new(1, 12)));
    assert_eq!(
        validate_rate(Decimal::new(1, 13)),
        Err(NumericError::OutOfRange)
    );
    // Trailing zeros are not places.
    assert_eq!(
        validate_rate(Decimal::new(10, 13)).unwrap().to_string(),
        "0.000000000001"
    );
    assert_eq!(
        validate_rate(dec("0.0000000000010")).unwrap().to_string(),
        "0.000000000001"
    );
}

#[test]
fn the_typed_threshold_and_quantity_validators_allow_scale_28() {
    let smallest = Decimal::new(1, 28);
    assert_eq!(validate_threshold(smallest), Ok(smallest));
    assert_eq!(validate_positive_quantity(smallest), Ok(smallest));
}

#[test]
fn the_typed_validators_treat_zero_like_the_parsers() {
    assert_eq!(validate_rate(Decimal::ZERO), Ok(Decimal::ZERO));
    assert_eq!(validate_threshold(Decimal::ZERO), Ok(Decimal::ZERO));
    assert_eq!(
        validate_positive_quantity(Decimal::ZERO),
        Err(NumericError::NotPositive)
    );
    assert_eq!(
        validate_positive_quantity(dec("0.000")),
        Err(NumericError::NotPositive)
    );
}

#[test]
fn a_negative_zero_is_zero_not_negative() {
    let negative_zero = -Decimal::ZERO;
    for validate in [validate_rate, validate_threshold] {
        let value = validate(negative_zero).unwrap();
        assert!(value.is_zero());
        assert!(!value.is_sign_negative());
    }
    assert_eq!(
        validate_positive_quantity(negative_zero),
        Err(NumericError::NotPositive)
    );
}

#[test]
fn a_minimum_fee_must_fit_the_currency_scale() {
    let eur = eur();
    assert_eq!(
        validate_minimum_fee(dec("12.340"), &eur)
            .unwrap()
            .to_string(),
        "12.34"
    );
    assert_eq!(
        validate_minimum_fee(dec("12.341"), &eur),
        Err(NumericError::InvalidMinimumFee)
    );
    assert_eq!(
        validate_minimum_fee(dec("0.047"), &eur),
        Err(NumericError::InvalidMinimumFee)
    );
    assert_eq!(
        validate_minimum_fee(dec("0.10"), &eur).unwrap().to_string(),
        "0.1"
    );
}

#[test]
fn a_minimum_fee_follows_the_scale_of_its_own_currency() {
    let jpy = CurrencySpec::try_new("JPY".into(), 0).unwrap();
    let kwd = CurrencySpec::try_new("KWD".into(), 3).unwrap();
    assert_eq!(validate_minimum_fee(dec("500.000"), &jpy), Ok(dec("500")));
    assert_eq!(
        validate_minimum_fee(dec("500.5"), &jpy),
        Err(NumericError::InvalidMinimumFee)
    );
    assert_eq!(validate_minimum_fee(dec("1.234"), &kwd), Ok(dec("1.234")));
    assert_eq!(
        validate_minimum_fee(dec("1.2345"), &kwd),
        Err(NumericError::InvalidMinimumFee)
    );
}

#[test]
fn a_minimum_fee_may_be_zero_but_not_negative() {
    let eur = eur();
    assert_eq!(validate_minimum_fee(Decimal::ZERO, &eur), Ok(Decimal::ZERO));
    assert!(
        !validate_minimum_fee(-Decimal::ZERO, &eur)
            .unwrap()
            .is_sign_negative()
    );
    assert_eq!(
        validate_minimum_fee(dec("-1"), &eur),
        Err(NumericError::Negative)
    );
    // A negative amount is Negative even when it also has too many places.
    assert_eq!(
        validate_minimum_fee(dec("-0.001"), &eur),
        Err(NumericError::Negative)
    );
}

#[test]
fn a_minimum_fee_outside_the_coefficient_limit_is_out_of_range() {
    let eur = eur();
    let largest = dec("9999999999999999999999999999");
    let next = Decimal::from_i128_with_scale(10_i128.pow(28), 0);
    assert_eq!(validate_minimum_fee(largest, &eur), Ok(largest));
    assert_eq!(
        validate_minimum_fee(next, &eur),
        Err(NumericError::OutOfRange)
    );
}

#[test]
fn every_error_has_its_own_message() {
    let all = [
        NumericError::InvalidText,
        NumericError::OutOfRange,
        NumericError::Negative,
        NumericError::NotPositive,
        NumericError::InvalidMinimumFee,
    ];
    let messages: std::collections::BTreeSet<String> =
        all.iter().map(ToString::to_string).collect();
    assert_eq!(messages.len(), all.len());
}

/// Text a wire client could send: any printable text, and decimal-like text around the limits
/// (more than 28 digits, more than 28 places, a sign).
fn wire_text() -> impl proptest::strategy::Strategy<Value = String> {
    proptest::prop_oneof!["\\PC{0,80}", "-?[0-9]{1,32}(\\.[0-9]{1,34})?"]
}

/// A value of at most `places` fractional places and at most 28 digits, with a mantissa of at
/// least `min`.
fn in_limit(places: u32, min: i128) -> impl proptest::strategy::Strategy<Value = Decimal> {
    use proptest::strategy::Strategy;
    (min..10_i128.pow(28), 0..=places)
        .prop_map(|(mantissa, scale)| Decimal::from_i128_with_scale(mantissa, scale))
}

proptest::proptest! {
    /// No parser panics on any text, and each refusal is one its doc names.
    #[test]
    fn any_text_parses_or_is_refused_with_a_documented_error(text in wire_text()) {
        use NumericError::{InvalidText, Negative, NotPositive, OutOfRange};
        let parsers: [(Parser, &[NumericError]); 3] = [
            (parse_rate, &[InvalidText, Negative, OutOfRange]),
            (parse_threshold, &[InvalidText, Negative, OutOfRange]),
            (parse_positive_quantity, &[InvalidText, Negative, NotPositive, OutOfRange]),
        ];
        for (parse, documented) in parsers {
            if let Err(error) = parse(&text) {
                proptest::prop_assert!(
                    documented.contains(&error),
                    "a refusal outside the parser's documented errors"
                );
            }
        }
    }

    /// A rate within its limits parses back from its canonical text.
    #[test]
    fn a_rate_round_trips_through_its_canonical_text(
        value in in_limit(RATE_FRACTIONAL_DIGITS, 0)
    ) {
        proptest::prop_assert_eq!(parse_rate(&bss_money::canonical_decimal(value)), Ok(value));
    }

    /// A threshold within its limits parses back from its canonical text.
    #[test]
    fn a_threshold_round_trips_through_its_canonical_text(
        value in in_limit(QUANTITY_FRACTIONAL_DIGITS, 0)
    ) {
        proptest::prop_assert_eq!(
            parse_threshold(&bss_money::canonical_decimal(value)),
            Ok(value)
        );
    }

    /// A positive quantity within its limits parses back from its canonical text.
    #[test]
    fn a_positive_quantity_round_trips_through_its_canonical_text(
        value in in_limit(QUANTITY_FRACTIONAL_DIGITS, 1)
    ) {
        proptest::prop_assert_eq!(
            parse_positive_quantity(&bss_money::canonical_decimal(value)),
            Ok(value)
        );
    }
}
