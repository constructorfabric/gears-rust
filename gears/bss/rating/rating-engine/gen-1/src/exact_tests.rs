#![allow(clippy::expect_used, clippy::unwrap_used)]

use rust_decimal::Decimal;

use super::{
    ArithmeticBudget, CoreCurrency, CoveredFraction, ExactMoney, ExactQuantity, QuantityTextError,
    REQUIRES_STRING,
};
use crate::outcome::MoneyText;

fn currency(code: &str, scale: u8) -> CoreCurrency {
    CoreCurrency {
        code: code.to_owned(),
        scale,
    }
}

fn price(text: &str, code: &str, scale: u8) -> ExactMoney {
    ExactMoney::from_price(text.parse::<Decimal>().unwrap(), &currency(code, scale))
}

fn wire(numerator: &str, denominator: &str, code: &str, scale: u8) -> MoneyText {
    MoneyText {
        numerator: numerator.to_owned(),
        denominator: denominator.to_owned(),
        currency: code.to_owned(),
        currency_scale: scale,
    }
}

fn q(text: &str) -> ExactQuantity {
    ExactQuantity::parse(text).unwrap()
}

const BUDGET: ArithmeticBudget = ArithmeticBudget::GEN_1;

#[test]
fn v30_pricing_money_stays_exact_in_major_units() {
    for (code, scale) in [("EUR", 2), ("JPY", 0), ("KWD", 3)] {
        let line = price("0.015", code, scale)
            .times_quantity(&q("12"), BUDGET, "v30")
            .unwrap();
        assert_eq!(line.to_wire("v30").unwrap(), wire("9", "50", code, scale));
    }
    assert_eq!(
        price("0.005", "EUR", 2).to_wire("v30").unwrap(),
        wire("1", "200", "EUR", 2)
    );
}

#[test]
fn v35_a_rate_below_the_posting_increment_stays_exact() {
    let line = price("0.047", "EUR", 2)
        .times_quantity(&q("2.5"), BUDGET, "v35")
        .unwrap();
    assert_eq!(line.to_wire("v35").unwrap(), wire("47", "400", "EUR", 2));
    let line = price("0.004", "EUR", 2)
        .times_quantity(&q("1"), BUDGET, "v35")
        .unwrap();
    assert_eq!(line.to_wire("v35").unwrap(), wire("1", "250", "EUR", 2));
}

#[test]
fn v38_a_third_of_a_period_is_one_third_and_three_thirds_are_one() {
    let third = price("1.00", "EUR", 2)
        .times_fraction(fraction(864_000, 2_592_000), BUDGET, "v38")
        .unwrap();
    assert_eq!(third.to_wire("v38").unwrap(), wire("1", "3", "EUR", 2));
    let whole = third
        .checked_add(&third, "v38")
        .and_then(|two| two.checked_add(&third, "v38"))
        .unwrap();
    assert_eq!(whole.to_wire("v38").unwrap(), wire("1", "1", "EUR", 2));
}

#[test]
fn v39_exact_beyond_rust_decimal_and_bounded_by_the_numerator_limit() {
    let tiny = price("0.0000000000000000000000000001", "EUR", 2)
        .times_quantity(&q("0.0000000000000000000000000001"), BUDGET, "v39")
        .unwrap();
    assert_eq!(
        tiny.to_wire("v39").unwrap(),
        wire("1", &format!("1{}", "0".repeat(56)), "EUR", 2)
    );

    let rate = price(&format!("1{}", "0".repeat(27)), "EUR", 2);
    let largest = rate
        .times_quantity(&q(&"9".repeat(57)), BUDGET, "v39")
        .unwrap();
    let numerator = format!("{}{}", "9".repeat(57), "0".repeat(27));
    assert_eq!(numerator.len(), 84);
    assert_eq!(
        largest.to_wire("v39").unwrap(),
        wire(&numerator, "1", "EUR", 2)
    );

    let beyond = rate
        .times_quantity(&q(&format!("1{}", "0".repeat(57))), BUDGET, "v39")
        .unwrap();
    let error = beyond.to_wire("v39").unwrap_err();
    assert_eq!(error.reason_code(), "precision_overflow");
}

#[test]
fn a_denominator_of_ten_to_the_64_is_beyond_the_bound() {
    let below = price("0.0000000000000000000000000001", "EUR", 2)
        .times_quantity(&q("0.0000000000000000000000000001"), BUDGET, "bound")
        .unwrap()
        .times_quantity(&q("0.0000001"), BUDGET, "bound")
        .unwrap();
    assert_eq!(below.to_wire("bound").unwrap().denominator.len(), 64);
    let at = below
        .times_quantity(&q("0.1"), BUDGET, "bound")
        .unwrap()
        .to_wire("bound")
        .unwrap_err();
    assert_eq!(at.reason_code(), "precision_overflow");
}

#[test]
fn v40_one_fraction_is_tagged_with_each_bindings_scale() {
    for (code, scale) in [("EUR", 2), ("KWD", 3)] {
        assert_eq!(
            price("0.125", code, scale).to_wire("v40").unwrap(),
            wire("1", "8", code, scale)
        );
    }
    assert_eq!(
        price("0.5", "JPY", 0).to_wire("v40").unwrap(),
        wire("1", "2", "JPY", 0)
    );
    assert_eq!(
        price("1.5", "JPY", 0).to_wire("v40").unwrap(),
        wire("3", "2", "JPY", 0)
    );
}

#[test]
fn v47_one_canonical_text_per_value() {
    assert_eq!(q("10.000"), q("10"));
    assert_eq!(q("10.000").as_str(), "10");
    assert_eq!(q("0.0470").as_str(), "0.047");
    assert_eq!(
        price("0.0470", "EUR", 2).to_wire("v47").unwrap(),
        price("0.047", "EUR", 2).to_wire("v47").unwrap()
    );
    let half = price("1", "EUR", 2)
        .times_fraction(fraction(2, 4), BUDGET, "v47")
        .unwrap();
    assert_eq!(half.to_wire("v47").unwrap(), wire("1", "2", "EUR", 2));
    assert_eq!(
        ExactMoney::zero(&currency("EUR", 2))
            .to_wire("v47")
            .unwrap(),
        wire("0", "1", "EUR", 2)
    );
    assert_eq!(q("0").as_str(), "0");
    for text in ["1e1", "+1", "01", "-0", "", "1.", ".5", "1,5", " 1"] {
        assert_eq!(
            ExactQuantity::parse(text),
            Err(QuantityTextError::NotCanonical),
            "{text:?}"
        );
    }
}

#[test]
fn a_quantity_beyond_its_digits_is_refused_by_name() {
    assert!(ExactQuantity::parse(&"9".repeat(60)).is_ok());
    assert_eq!(
        ExactQuantity::parse(&"9".repeat(61)),
        Err(QuantityTextError::OutOfRange)
    );
    assert_eq!(
        ExactQuantity::parse(&format!("0.{}", "1".repeat(29))),
        Err(QuantityTextError::OutOfRange)
    );
    let sum = q(&"9".repeat(60)).checked_add(&q("1"), "sum").unwrap_err();
    assert_eq!(sum.reason_code(), "quantity_out_of_range");
    let third = bss_money::exact::ExactAmount::from_coefficient(1, 0)
        .checked_div(&bss_money::exact::ExactAmount::from_coefficient(3, 0))
        .unwrap();
    assert_eq!(
        ExactQuantity::from_exact(&third, "third")
            .unwrap_err()
            .reason_code(),
        "quantity_out_of_range"
    );
}

#[test]
fn quantities_cross_the_wire_as_strings_only() {
    let parsed: ExactQuantity = serde_json::from_str(r#""2.50""#).unwrap();
    assert_eq!(serde_json::to_string(&parsed).unwrap(), r#""2.5""#);
    let number = serde_json::from_str::<ExactQuantity>("2.5").unwrap_err();
    assert!(number.to_string().contains(REQUIRES_STRING), "{number}");
}

#[test]
fn quantities_round_trip_through_exact_values() {
    for text in [
        "0",
        "-1.25",
        "12",
        &"9".repeat(60),
        &format!("0.{}1", "0".repeat(27)),
    ] {
        let quantity = q(text);
        let back = ExactQuantity::from_exact(&quantity.to_exact().unwrap(), "round trip");
        assert_eq!(back.unwrap(), quantity, "{text}");
    }
}

#[test]
fn amounts_of_another_currency_or_scale_never_meet() {
    let eur = price("1", "EUR", 2);
    for other in [price("1", "USD", 2), price("1", "EUR", 3)] {
        let error = eur.checked_add(&other, "sum").unwrap_err();
        assert_eq!(error.reason_code(), "binding_currency_mismatch");
        let error = eur.checked_sub(&other, "difference").unwrap_err();
        assert_eq!(error.reason_code(), "binding_currency_mismatch");
    }
}

#[test]
fn an_empty_period_has_no_covered_fraction() {
    let error = price("1", "EUR", 2)
        .times_fraction(fraction(1, 0), BUDGET, "proration")
        .unwrap_err();
    assert_eq!(error.reason_code(), "period_geometry_mismatch");
}

/// `covered / period` as a covered fraction.
fn fraction(covered_seconds: u64, period_seconds: u64) -> CoveredFraction {
    CoveredFraction {
        covered_seconds,
        period_seconds,
    }
}

#[test]
fn a_wire_amount_is_canonical_reduced_text() {
    let wire = |numerator: &str, denominator: &str| MoneyText {
        numerator: numerator.to_owned(),
        denominator: denominator.to_owned(),
        currency: "EUR".to_owned(),
        currency_scale: 2,
    };
    for (numerator, denominator) in [("47", "400"), ("-3", "2"), ("0", "1")] {
        let amount = ExactMoney::from_wire(&wire(numerator, denominator)).unwrap();
        assert_eq!(amount.to_wire("t").unwrap(), wire(numerator, denominator));
    }
    for (numerator, denominator) in [
        ("-0", "1"),
        ("007", "1"),
        ("+1", "1"),
        ("-", "1"),
        ("", "1"),
        ("1", "0"),
        ("1", "-2"),
        ("1", "02"),
        ("2", "4"),
    ] {
        let error = ExactMoney::from_wire(&wire(numerator, denominator)).unwrap_err();
        assert_eq!(
            error.reason_code(),
            "money_contract_violation",
            "{numerator}/{denominator}"
        );
        // Decoding a wire amount checks the same form.
        let json = serde_json::to_string(&wire(numerator, denominator)).unwrap();
        assert!(serde_json::from_str::<MoneyText>(&json).is_err(), "{json}");
    }
}

#[test]
fn quantity_text_is_read_up_to_128_bytes() {
    let padded = |len: usize| format!("1.{}", "0".repeat(len - 2));
    assert_eq!(ExactQuantity::parse(&padded(128)).unwrap().as_str(), "1");
    assert_eq!(
        ExactQuantity::parse(&padded(129)),
        Err(QuantityTextError::NotCanonical)
    );
}

proptest::proptest! {
    /// Parse, write and parse again give one text, and the exact value round-trips.
    #[test]
    fn quantities_round_trip(whole in "[1-9][0-9]{0,40}|0", fraction in "[0-9]{0,28}", negative: bool) {
        let text = if fraction.is_empty() {
            whole
        } else {
            format!("{whole}.{fraction}")
        };
        let text = if negative && text.trim_matches(['0', '.']).is_empty() {
            text
        } else if negative {
            format!("-{text}")
        } else {
            text
        };
        let quantity = ExactQuantity::parse(&text).unwrap();
        let again = ExactQuantity::parse(quantity.as_str()).unwrap();
        proptest::prop_assert_eq!(&again, &quantity);
        let exact = quantity.to_exact().unwrap();
        proptest::prop_assert_eq!(ExactQuantity::from_exact(&exact, "t").unwrap(), quantity);
    }

    /// No text makes the parser panic.
    #[test]
    fn any_quantity_text_is_read_or_refused(text in ".{0,160}") {
        drop(ExactQuantity::parse(&text));
    }
}
