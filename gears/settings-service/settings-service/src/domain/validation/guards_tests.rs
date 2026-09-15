// Created: 2026-09-06 by Constructor Tech
//! Tests for the size and canonicality guards.

use serde_json::json;

use super::{MAX_SERIALIZED_BYTES, check, check_text};
use crate::field;

#[test]
fn a_value_at_the_cap_passes_and_one_byte_over_is_too_large() {
    // The cap is a hard bound, not a soft one: a value one byte over is refused
    // outright so nothing above it exists to plan capacity for.
    let at_cap = json!("x".repeat(MAX_SERIALIZED_BYTES - 2)); // two quotes
    assert!(check(&at_cap).is_ok());
    let over = json!("x".repeat(MAX_SERIALIZED_BYTES - 1));
    let err = check(&over).expect_err("over the cap");
    assert_eq!(err.code, field::VALUE_TOO_LARGE);
    assert_eq!(err.field, "value");
}

#[test]
fn an_integer_within_the_double_range_is_canonical() {
    assert!(check(&json!(9_007_199_254_740_992_u64)).is_ok()); // 2^53 exactly
    assert!(check(&json!(-9_007_199_254_740_992_i64)).is_ok());
    assert!(check(&json!(42)).is_ok());
}

#[test]
fn an_integer_beyond_the_double_range_is_not_canonical() {
    let err = check(&json!(9_007_199_254_740_993_u64)).expect_err("2^53 + 1");
    assert_eq!(err.code, field::VALUE_NOT_CANONICAL);
    assert_eq!(err.field, "value");
    assert!(check(&json!(-9_007_199_254_740_993_i64)).is_err());
}

#[test]
fn a_nested_number_is_checked_and_its_position_named() {
    // The position matters: an administrator fixing a structured value needs
    // to know which leaf failed, not that "the value" did.
    let err = check(&json!({ "limits": [1, { "max": 9_007_199_254_740_993_u64 }] }))
        .expect_err("nested overflow");
    assert_eq!(err.field, "value/limits/1/max");
    assert_eq!(err.code, field::VALUE_NOT_CANONICAL);
}

#[test]
fn text_with_a_decimal_finer_than_a_double_is_not_canonical() {
    // 0.1 followed by digits a double cannot hold: parsed, it becomes 0.1 and
    // the parsed-value guard could never tell; the text guard can.
    let err = check_text(r#"{"ratio": 0.10000000000000000555}"#).expect_err("finer than a double");
    assert_eq!(err.code, field::VALUE_NOT_CANONICAL);
    assert!(
        err.message.contains("0.10000000000000000555"),
        "{}",
        err.message
    );
}

#[test]
fn text_with_ordinary_decimals_and_exponents_is_canonical() {
    for text in [
        r#"{"a": 0.1, "b": 1.5, "c": -2.25}"#,
        r"[1e2, 1.0, 100, 2.5E-3]",
        r#"{"note": "0.10000000000000000555 in a string is not a number"}"#,
        r#"{"escaped": "a \" quote then 9007199254740993 inside the string"}"#,
    ] {
        assert!(check_text(text).is_ok(), "{text}");
    }
}

#[test]
fn text_with_an_integer_beyond_the_double_range_is_not_canonical() {
    let err = check_text("[1, 9007199254740993]").expect_err("2^53 + 1 in text");
    assert_eq!(err.code, field::VALUE_NOT_CANONICAL);
}

#[test]
fn text_over_the_cap_is_too_large_before_any_number_is_read() {
    let text = format!("\"{}\"", "x".repeat(MAX_SERIALIZED_BYTES));
    assert_eq!(
        check_text(&text).expect_err("over the cap").code,
        field::VALUE_TOO_LARGE
    );
}
