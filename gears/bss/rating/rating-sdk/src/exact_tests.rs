#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;

fn money(json: &str) -> Result<ExactMoney, serde_json::Error> {
    serde_json::from_str(json)
}

#[test]
fn a_fraction_round_trips_as_canonical_strings() {
    let amount = ExactMoney::new("1", "200", "EUR", 2).unwrap();
    let json = serde_json::to_string(&amount).unwrap();
    assert_eq!(
        json,
        r#"{"numerator":"1","denominator":"200","currency":"EUR","currency_scale":2}"#
    );
    assert_eq!(money(&json).unwrap(), amount);
}

#[test]
fn a_negative_amount_carries_its_sign_on_the_numerator() {
    let amount = ExactMoney::new("-47", "400", "EUR", 2).unwrap();
    assert_eq!(
        serde_json::to_string(&amount).unwrap(),
        r#"{"numerator":"-47","denominator":"400","currency":"EUR","currency_scale":2}"#
    );
    // The sign belongs to the numerator only.
    assert!(
        money(r#"{"numerator":"47","denominator":"-400","currency":"EUR","currency_scale":2}"#)
            .is_err()
    );
}

#[test]
fn a_json_number_in_a_money_field_requires_a_string() {
    let err = money(r#"{"numerator":1,"denominator":"200","currency":"EUR","currency_scale":2}"#)
        .unwrap_err();
    assert!(err.to_string().contains(REQUIRES_STRING), "{err}");
    let err = money(r#"{"numerator":"1","denominator":2.5,"currency":"EUR","currency_scale":2}"#)
        .unwrap_err();
    assert!(err.to_string().contains(REQUIRES_STRING), "{err}");
}

#[test]
fn the_scale_stays_a_json_integer() {
    assert!(
        money(r#"{"numerator":"1","denominator":"2","currency":"EUR","currency_scale":"2"}"#)
            .is_err()
    );
}

#[test]
fn non_canonical_integers_are_refused() {
    for numerator in ["+1", "01", "-0", "1e3", "1.0", "1_000", " 1", "", "--1"] {
        assert!(
            matches!(
                ExactMoney::new(numerator, "1", "EUR", 2),
                Err(ExactNumberError::NotCanonical { field: "numerator" })
            ),
            "{numerator:?} must be refused"
        );
    }
}

#[test]
fn the_denominator_is_positive_and_zero_is_zero_over_one() {
    assert!(matches!(
        ExactMoney::new("1", "0", "EUR", 2),
        Err(ExactNumberError::InvalidFraction { .. })
    ));
    assert!(matches!(
        ExactMoney::new("1", "-2", "EUR", 2),
        Err(ExactNumberError::InvalidFraction { .. })
    ));
    assert!(matches!(
        ExactMoney::new("0", "5", "EUR", 2),
        Err(ExactNumberError::InvalidFraction { .. })
    ));
    assert!(ExactMoney::new("0", "1", "EUR", 2).is_ok());
}

#[test]
fn the_delivery_bounds_hold_at_their_edges() {
    let max_numerator = "9".repeat(MAX_NUMERATOR_DIGITS);
    assert!(ExactMoney::new(&max_numerator, "1", "EUR", 2).is_ok());
    let negative = format!("-{max_numerator}");
    assert!(ExactMoney::new(&negative, "1", "EUR", 2).is_ok());
    let too_big = format!("1{}", "0".repeat(MAX_NUMERATOR_DIGITS));
    assert!(matches!(
        ExactMoney::new(&too_big, "1", "EUR", 2),
        Err(ExactNumberError::OutOfRange {
            field: "numerator",
            ..
        })
    ));
    let max_denominator = "9".repeat(MAX_DENOMINATOR_DIGITS);
    assert!(ExactMoney::new("1", &max_denominator, "EUR", 2).is_ok());
    let too_big = format!("1{}", "0".repeat(MAX_DENOMINATOR_DIGITS));
    assert!(matches!(
        ExactMoney::new("1", &too_big, "EUR", 2),
        Err(ExactNumberError::OutOfRange {
            field: "denominator",
            ..
        })
    ));
}

#[test]
fn the_currency_follows_the_money_crate_rules() {
    let refused = |code: &str, scale: u8| {
        matches!(
            ExactMoney::new("1", "1", code, scale),
            Err(ExactNumberError::InvalidCurrency { .. })
        )
    };
    assert!(refused("eur", 2));
    assert!(refused("", 2));
    assert!(refused("EUR", 29));
    assert!(refused(&"A".repeat(17), 2));
    assert!(ExactMoney::new("1", "1", "JPY", 0).is_ok());
    assert!(ExactMoney::new("1", "1", "KWD", 3).is_ok());
    // At the edges: a 16-character code and scale 28.
    assert!(ExactMoney::new("1", "1", &"A".repeat(16), 28).is_ok());
}

#[test]
fn a_member_a_later_v1_release_adds_is_ignored() {
    let amount =
        money(r#"{"numerator":"1","denominator":"2","currency":"EUR","currency_scale":2,"x":1}"#)
            .unwrap();
    assert_eq!(amount, ExactMoney::new("1", "2", "EUR", 2).unwrap());
}

#[test]
fn only_reduced_fractions_are_accepted() {
    for (numerator, denominator) in [("2", "4"), ("-6", "8"), ("10", "5"), ("3", "3")] {
        assert!(
            matches!(
                ExactMoney::new(numerator, denominator, "EUR", 2),
                Err(ExactNumberError::InvalidFraction {
                    reason: "the fraction is not reduced",
                    ..
                })
            ),
            "{numerator}/{denominator} must be refused"
        );
    }
    for (numerator, denominator) in [("1", "2"), ("-3", "4"), ("0", "1"), ("7", "1")] {
        assert!(
            ExactMoney::new(numerator, denominator, "EUR", 2).is_ok(),
            "{numerator}/{denominator} must be accepted"
        );
    }
    // At the bounds: 10^84 - 1 is coprime to 10^63; an even numerator over 10^63 is not.
    let max_numerator = "9".repeat(MAX_NUMERATOR_DIGITS);
    let max_denominator = format!("1{}", "0".repeat(MAX_DENOMINATOR_DIGITS - 1));
    assert!(ExactMoney::new(&max_numerator, &max_denominator, "EUR", 2).is_ok());
    assert!(ExactMoney::new(&format!("-{max_numerator}"), &max_denominator, "EUR", 2).is_ok());
    let even = format!("{}8", "9".repeat(MAX_NUMERATOR_DIGITS - 1));
    assert!(matches!(
        ExactMoney::new(&even, &max_denominator, "EUR", 2),
        Err(ExactNumberError::InvalidFraction { .. })
    ));
    let err = money(r#"{"numerator":"2","denominator":"4","currency":"EUR","currency_scale":2}"#)
        .unwrap_err();
    assert!(err.to_string().contains("not reduced"), "{err}");
}

#[test]
fn quantities_drop_fractional_trailing_zeros() {
    for (input, canonical) in [
        ("10.000", "10"),
        ("10.0", "10"),
        ("10", "10"),
        ("0.500", "0.5"),
        ("-3.25", "-3.25"),
        ("0", "0"),
        ("0.000", "0"),
    ] {
        assert_eq!(ExactQuantity::parse(input).unwrap().as_str(), canonical);
    }
}

#[test]
fn non_canonical_quantities_are_refused() {
    for input in [
        "+1", "01", "00.5", ".5", "5.", "1e3", "1E3", "-0", "-0.000", "1,000", "1 000", "", "-",
        "0x10", "NaN", "inf",
    ] {
        assert!(
            matches!(
                ExactQuantity::parse(input),
                Err(ExactNumberError::NotCanonical { .. })
            ),
            "{input:?} must be refused"
        );
    }
}

#[test]
fn quantity_bounds_are_sixty_and_twenty_eight_digits() {
    let integer = "9".repeat(MAX_QUANTITY_INTEGER_DIGITS);
    let fraction = "9".repeat(MAX_QUANTITY_FRACTION_DIGITS);
    assert!(ExactQuantity::parse(&format!("{integer}.{fraction}")).is_ok());
    assert!(matches!(
        ExactQuantity::parse(&format!("1{integer}")),
        Err(ExactNumberError::OutOfRange { .. })
    ));
    assert!(matches!(
        ExactQuantity::parse(&format!("0.{fraction}1")),
        Err(ExactNumberError::OutOfRange { .. })
    ));
    // Trailing zeros beyond 28 places are dropped before the bound is checked.
    assert_eq!(
        ExactQuantity::parse(&format!("0.{fraction}000"))
            .unwrap()
            .as_str(),
        format!("0.{fraction}")
    );
}

#[test]
fn a_json_number_in_a_quantity_field_requires_a_string() {
    let err = serde_json::from_str::<ExactQuantity>("10").unwrap_err();
    assert!(err.to_string().contains(REQUIRES_STRING), "{err}");
    let err = serde_json::from_str::<ExactQuantity>("10.5").unwrap_err();
    assert!(err.to_string().contains(REQUIRES_STRING), "{err}");
    let quantity: ExactQuantity = serde_json::from_str(r#""10.000""#).unwrap();
    assert_eq!(serde_json::to_string(&quantity).unwrap(), r#""10""#);
}

#[test]
fn oversized_input_is_refused_before_it_is_scanned() {
    // Canonical once its trailing zeros are dropped, but longer than any input is read.
    let long_zero = format!("0.{}", "0".repeat(MAX_INPUT_LEN - 1));
    assert!(matches!(
        ExactQuantity::parse(&long_zero),
        Err(ExactNumberError::NotCanonical { .. })
    ));
    assert!(ExactQuantity::parse(&long_zero[..MAX_INPUT_LEN]).is_ok());
    let long = "1".repeat(MAX_INPUT_LEN + 1);
    assert!(ExactMoney::new(&long, "1", "EUR", 2).is_err());
}

mod properties {
    use proptest::prelude::*;

    use super::*;

    /// Canonical decimal text within the quantity bounds.
    fn canonical_quantity() -> impl Strategy<Value = String> {
        (
            any::<bool>(),
            "(0|[1-9][0-9]{0,59})",
            proptest::option::of("[0-9]{0,27}[1-9]"),
        )
            .prop_filter_map("-0 is not canonical", |(negative, integer, fraction)| {
                let mut text = String::new();
                if negative {
                    if integer == "0" && fraction.is_none() {
                        return None;
                    }
                    text.push('-');
                }
                text.push_str(&integer);
                if let Some(fraction) = fraction {
                    text.push('.');
                    text.push_str(&fraction);
                }
                Some(text)
            })
    }

    proptest! {
        #[test]
        fn any_text_is_refused_or_canonical_and_never_panics(text in "\\PC{0,80}") {
            if let Ok(quantity) = ExactQuantity::parse(&text) {
                // What parses reads back as itself.
                prop_assert_eq!(ExactQuantity::parse(quantity.as_str()).unwrap(), quantity);
            }
            // Arbitrary text is refused or accepted, never a panic.
            let _numerator = ExactMoney::new(&text, "1", "EUR", 2).is_ok();
            let _denominator = ExactMoney::new("1", &text, "EUR", 2).is_ok();
        }

        #[test]
        fn canonical_quantities_pass_unchanged_and_round_trip(text in canonical_quantity()) {
            let quantity = ExactQuantity::parse(&text).unwrap();
            prop_assert_eq!(quantity.as_str(), text.as_str());
            let json = serde_json::to_string(&quantity).unwrap();
            prop_assert_eq!(serde_json::from_str::<ExactQuantity>(&json).unwrap(), quantity);
        }

        #[test]
        fn trailing_zeros_never_change_a_quantity(text in canonical_quantity(), zeros in 1usize..8) {
            let padded = if text.contains('.') {
                format!("{text}{}", "0".repeat(zeros))
            } else {
                format!("{text}.{}", "0".repeat(zeros))
            };
            let quantity = ExactQuantity::parse(&padded).unwrap();
            prop_assert_eq!(quantity.as_str(), text.as_str());
        }

        #[test]
        fn accepted_amounts_round_trip(numerator in -1_000_000_000_i64..1_000_000_000, denominator in 1_i64..1_000_000) {
            match ExactMoney::new(&numerator.to_string(), &denominator.to_string(), "EUR", 2) {
                Ok(amount) => {
                    let json = serde_json::to_string(&amount).unwrap();
                    prop_assert_eq!(serde_json::from_str::<ExactMoney>(&json).unwrap(), amount);
                }
                // Refused only when not reduced (zero is 0/1).
                Err(err) => prop_assert!(
                    matches!(err, ExactNumberError::InvalidFraction { .. }),
                    "{numerator}/{denominator}: {err}"
                ),
            }
        }
    }
}
