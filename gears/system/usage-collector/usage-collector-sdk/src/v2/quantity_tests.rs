use std::str::FromStr;

use rust_decimal::Decimal;
use serde_json::json;

use super::UsageQuantity;
use crate::v2::error::UsageCollectorError;
use crate::v2::reason::ValidationReason;

fn assert_out_of_range(text: &str) {
    let err = UsageQuantity::parse(text).expect_err(text);
    assert!(
        matches!(
            &err,
            UsageCollectorError::InvalidArgument { field, reason: ValidationReason::QuantityOutOfRange, .. }
                if field == "quantity"
        ),
        "`{text}` must be QUANTITY_OUT_OF_RANGE on `quantity`, got {err:?}",
    );
}

#[test]
fn accepted_forms_render_back_digit_for_digit() {
    for text in [
        "0",
        "0.000",
        "42",
        "42.500",
        "-42.5",
        "9999999999999999999999999999",
        "-9999999999999999999999999999",
        "0.0000000000000000000000000001",
        "-0.0000000000000000000000000001",
        "1.234567890123456789012345678",
        "12345678901234567890.12345678",
    ] {
        let quantity = UsageQuantity::parse(text).unwrap_or_else(|e| unreachable!("`{text}`: {e}"));
        assert_eq!(
            quantity.to_string(),
            text,
            "`{text}` must render back unchanged"
        );
    }
}

#[test]
fn forms_outside_the_published_pattern_are_rejected() {
    for text in [
        "",
        "-",
        "+1",
        "1e3",
        "1E3",
        "1_000",
        "01",
        "-01",
        "00.5",
        ".5",
        "5.",
        "1.2.3",
        " 1",
        "1 ",
        "0x10",
        "NaN",
        "inf",
        // 29 integer digits.
        "10000000000000000000000000000",
        // 29 fraction digits.
        "0.00000000000000000000000000001",
    ] {
        assert_out_of_range(text);
    }
}

#[test]
fn more_than_28_significant_digits_is_rejected_even_inside_the_pattern() {
    // 28 integer digits + 1 fraction digit = 29 significant digits.
    assert_out_of_range("9999999999999999999999999999.9");
    // Trailing zeros are significant: 1 followed by 28 zeros after the point.
    assert_out_of_range("1.0000000000000000000000000000");
    // Leading zeros are not: this is one significant digit.
    UsageQuantity::parse("0.0000000000000000000000000009").expect("one significant digit");
    // Exactly 28.
    UsageQuantity::parse("1.234567890123456789012345678").expect("28 significant digits");
}

#[test]
fn negative_zero_is_rejected_because_it_cannot_round_trip() {
    for text in ["-0", "-0.0", "-0.0000000000000000000000000000"] {
        assert_out_of_range(text);
    }
}

#[test]
fn equality_is_textual_and_as_decimal_exposes_the_numeric_value() {
    let short = UsageQuantity::parse("42.5").unwrap();
    let long = UsageQuantity::parse("42.500").unwrap();
    assert_ne!(
        short, long,
        "equality is digit for digit: 42.5 and 42.500 are two quantities"
    );
    assert_eq!(short, UsageQuantity::parse("42.5").unwrap());
    assert_eq!(
        short.as_decimal(),
        long.as_decimal(),
        "the numeric value stays reachable through as_decimal"
    );
    assert_eq!(long.as_decimal(), Decimal::from_str("42.5").unwrap());
}

#[test]
fn hashing_agrees_with_textual_equality() {
    let set: std::collections::HashSet<UsageQuantity> = ["42.5", "42.500", "42.5", "-1", "0"]
        .into_iter()
        .map(|text| UsageQuantity::parse(text).unwrap())
        .collect();
    assert_eq!(
        set.len(),
        4,
        "equal texts hash together, differently scaled ones apart"
    );
}

#[test]
fn from_str_and_try_from_route_through_parse() {
    assert_eq!(UsageQuantity::from_str("7").unwrap().to_string(), "7");
    assert_eq!(UsageQuantity::try_from("7.10").unwrap().to_string(), "7.10");
    assert!(UsageQuantity::from_str("-0").is_err());
    assert!(UsageQuantity::try_from("1e3").is_err());
}

#[test]
fn try_from_decimal_preserves_scale_and_validates() {
    let d = Decimal::from_str("42.500").unwrap();
    assert_eq!(UsageQuantity::try_from(d).unwrap().to_string(), "42.500");
    // rust_decimal carries 29 significant digits; the published bound is 28.
    let too_wide = Decimal::MAX;
    assert!(UsageQuantity::try_from(too_wide).is_err());
    // A sign-flagged zero `Decimal` (unlike `Decimal::from_str_exact("-0")`,
    // which normalizes the sign away) is a negative sign over a zero
    // mantissa — what the text path reads off a leading `-`, same rejection.
    assert!(UsageQuantity::try_from(-Decimal::ZERO).is_err());
}

/// `TryFrom<Decimal>` reads the bounds off the carrier instead of rendering
/// and re-parsing, so nothing but a test pins it to the text path. Every
/// value must reach the same verdict — and the same message on a rejection —
/// through both doors. Sign, scale and digit width vary independently,
/// because that is the decomposition the carrier-side check reads.
#[test]
fn try_from_decimal_agrees_with_the_text_path_value_for_value() {
    let twenty_eight_nines = "9".repeat(28);
    let cases: Vec<Decimal> = [
        "0",
        "0.0",
        "1",
        "-1",
        "42.500",
        "-42.500",
        // Below one: the integer part is a rendered `0`, not a digit.
        "0.1",
        "-0.0000000000000000000000000001",
        // The fraction width at its 28-digit bound.
        "0.0000000000000000000000000001",
        &twenty_eight_nines,
        &format!("-{twenty_eight_nines}"),
    ]
    .into_iter()
    .map(|text| Decimal::from_str(text).expect("a representable carrier value"))
    .chain([
        Decimal::ZERO,
        -Decimal::ZERO,
        // 29 significant digits, over the bound and reachable only as a
        // carrier value (29 nines do not fit, so these are the widest).
        Decimal::MAX,
        Decimal::MIN,
        // 29 digits under a 28-digit integer part: the one shape that
        // clears the integer bound and is still refused for its width, so
        // the two rejections differ by more than the integer part.
        Decimal::try_from_i128_with_scale(Decimal::MAX.mantissa(), 1)
            .expect("the carrier's widest mantissa at scale 1"),
    ])
    .collect();

    for value in cases {
        let through_text = UsageQuantity::parse(&value.to_string());
        let through_carrier = UsageQuantity::try_from(value);
        match (through_text, through_carrier) {
            (Ok(text), Ok(carrier)) => assert_eq!(
                text, carrier,
                "`{value}` must decode to one quantity through either door"
            ),
            (Err(text), Err(carrier)) => assert_eq!(
                text.to_string(),
                carrier.to_string(),
                "`{value}` must be refused with one message through either door"
            ),
            (text, carrier) => {
                unreachable!(
                    "`{value}` reached opposite verdicts: text {text:?}, carrier {carrier:?}"
                )
            }
        }
    }
}

#[test]
fn serde_is_a_json_string_only() {
    let q = UsageQuantity::parse("42.500").unwrap();
    assert_eq!(serde_json::to_value(q).unwrap(), json!("42.500"));
    assert_eq!(
        serde_json::from_value::<UsageQuantity>(json!("42.500")).unwrap(),
        q
    );
    serde_json::from_value::<UsageQuantity>(json!(42.5)).expect_err("a JSON number is refused");
    serde_json::from_value::<UsageQuantity>(json!("1e3")).expect_err("routes through parse");
}

/// The two rule implementations, crossed against each other.
///
/// `check_text` reads the wire string and `check_decimal` reads the carrier,
/// and a fixed table can only cross them at the values it lists. These pin
/// the properties a table cannot: one verdict and one message through both
/// doors, and no panic on arbitrary input.
mod rules_agree_through_both_doors {
    use proptest::prelude::*;
    use rust_decimal::Decimal;

    use crate::v2::quantity::UsageQuantity;

    /// `Decimal`'s 96-bit mantissa ceiling, 29 digits.
    const MAX_MANTISSA: i128 = 79_228_162_514_264_337_593_543_950_335;

    proptest! {
        /// A `Decimal` and its own rendering must reach the same verdict,
        /// and the same message when they refuse.
        #[test]
        fn a_decimal_and_its_rendering_reach_one_verdict(
            // The carrier's own mantissa range, so the generator lands on
            // representable values instead of rejecting ~all of them. Its
            // upper end is 29 digits, which is what reaches the
            // significant-digit refusal the 28-digit bound owns.
            mantissa in -MAX_MANTISSA..=MAX_MANTISSA,
            scale in 0_u32..=28,
        ) {
            let Ok(value) = Decimal::try_from_i128_with_scale(mantissa, scale) else {
                return Ok(());
            };
            let text = value.to_string();

            let via_carrier = UsageQuantity::try_from(value).map_err(|e| e.to_string());
            let via_text = UsageQuantity::parse(&text).map_err(|e| e.to_string());

            prop_assert_eq!(
                via_carrier.is_ok(),
                via_text.is_ok(),
                "disagreed on `{}`: carrier {:?}, text {:?}",
                text, via_carrier, via_text,
            );
            prop_assert_eq!(&via_carrier, &via_text, "same verdict, different message");
            if let Ok(parsed) = via_text {
                prop_assert_eq!(parsed.as_decimal(), value);
            }
        }

        /// Arbitrary text never panics, and whatever it accepts round-trips
        /// digit for digit - the equality this type publishes.
        #[test]
        fn arbitrary_text_never_panics_and_accepted_text_round_trips(text in ".{0,80}") {
            if let Ok(quantity) = UsageQuantity::parse(&text) {
                prop_assert_eq!(quantity.to_string(), text);
            }
        }

        /// The same, over strings drawn from the alphabet the pattern uses,
        /// so the generator actually reaches the accepting path.
        #[test]
        fn pattern_shaped_text_never_panics(text in "-?[0-9]{0,32}(\\.[0-9]{0,32})?") {
            if let Ok(quantity) = UsageQuantity::parse(&text) {
                prop_assert_eq!(quantity.to_string(), text);
            }
        }
    }
}
