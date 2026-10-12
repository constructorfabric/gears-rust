//! Exact positive provider quote regression vectors and contract bounds.

use super::parse_rate;
use bss_ledger_sdk::RateProviderError;

#[test]
fn preserves_provider_precision_regression() {
    for text in [
        "1.123456789",
        "0.0000001",
        "0.0000015",
        "0.0000025",
        "0.0000035",
        "1.0856",
        "160.85",
    ] {
        assert_eq!(parse_rate(text).unwrap().to_string(), text);
    }
}

#[test]
fn positive_bounds_are_shared_with_money_parser() {
    for text in [
        "9999999999999999999999999999",
        "0.0000000000000000000000000001",
        "9223372036854.775808",
        "100000000000000000000000",
    ] {
        assert_eq!(parse_rate(text).unwrap().to_string(), text);
    }
    assert_eq!(
        parse_rate("1.123456789000").unwrap().to_string(),
        "1.123456789"
    );
    assert_eq!(
        parse_rate("1.00000000000000000000000000000")
            .unwrap()
            .to_string(),
        "1"
    );
}

#[test]
fn rejects_invalid_nonpositive_and_out_of_contract_quotes() {
    for text in [
        "abc",
        "NaN",
        "Infinity",
        "0",
        "0.000000",
        "-0",
        "-1.5",
        "-0.0000001",
        "1e-7",
        "",
        "10000000000000000000000000000",
        "1.0000000000000000000000000001",
        "0.00000000000000000000000000001",
    ] {
        assert!(
            matches!(parse_rate(text), Err(RateProviderError::Internal(_))),
            "accepted {text}"
        );
    }
    assert!(parse_rate(&format!("1.{}", "0".repeat(63))).is_err());
}

#[test]
fn preserves_legacy_exact_parser_lexical_forms() {
    for (text, expected) in [
        (" 1.123456789 ", "1.123456789"),
        ("\t+01.123456789\n", "1.123456789"),
        ("000.0000001", "0.0000001"),
        (".125", "0.125"),
        ("1.", "1"),
        ("1_234.5_6", "1234.56"),
    ] {
        // Assert these are accepted by the previous provider exact parser.
        let legacy = rust_decimal::Decimal::from_str_exact(text.trim()).unwrap();
        assert_eq!(legacy.normalize().to_string(), expected);
        assert_eq!(parse_rate(text).unwrap().to_string(), expected);
    }
}

#[test]
fn lexical_normalization_does_not_bypass_bounds_or_positivity() {
    for text in [
        " +000 ",
        " -.0000001 ",
        " +10000000000000000000000000000 ",
        " +01.0000000000000000000000000001 ",
    ] {
        assert!(parse_rate(text).is_err(), "accepted {text}");
    }
    assert!(parse_rate(&format!("{}1", " ".repeat(64))).is_err());
}

#[test]
fn a_quote_at_the_64_byte_limit_is_accepted_and_errors_name_the_text() {
    let at_limit = format!("1.{}", "0".repeat(62));
    assert_eq!(at_limit.len(), 64);
    assert_eq!(parse_rate(&at_limit).unwrap(), rust_decimal::Decimal::ONE);
    let padded = format!(" 1.{}", "0".repeat(61));
    assert_eq!(padded.len(), 64);
    assert_eq!(parse_rate(&padded).unwrap(), rust_decimal::Decimal::ONE);
    assert!(parse_rate(&format!("{at_limit}0")).is_err(), "65 bytes");
    for text in ["abc", "-1.5", "0"] {
        let Err(RateProviderError::Internal(message)) = parse_rate(text) else {
            panic!("{text:?} must be refused");
        };
        assert!(message.contains(&format!("{text:?}")), "{message}");
    }
}
