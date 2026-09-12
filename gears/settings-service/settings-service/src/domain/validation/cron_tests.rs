// Created: 2026-09-08 by Constructor Tech
//! What the dialect admits, and what it refuses with a reason.

use super::{is_known, parse};

#[test]
fn every_shape_the_standard_dialect_admits_parses() {
    for expression in [
        "* * * * *",
        "0 0 * * *",
        "*/15 * * * *",
        "0 9-17 * * 1-5",
        "0,30 8,12,18 1 * *",
        "0 0 1-28/7 * *",
        "0 0 * jan,jul mon",
        "0 0 * JAN-DEC SUN",
        "59 23 31 12 7",
        "  0 0 * * *  ",
    ] {
        assert!(parse(expression).is_ok(), "{expression} should parse");
    }
}

#[test]
fn a_field_count_other_than_five_is_refused_by_count() {
    for expression in ["", "   ", "* * * *", "* * * * * *", "0 0 * * * 2026"] {
        let refusal = parse(expression).expect_err("refused");
        assert!(
            refusal.contains("five fields") || refusal.contains("empty"),
            "{expression}: {refusal}"
        );
    }
}

#[test]
fn a_value_outside_its_field_is_refused_and_the_field_is_named() {
    for (expression, field) in [
        ("60 0 * * *", "minute"),
        ("0 24 * * *", "hour"),
        ("0 0 32 * *", "day of month"),
        ("0 0 0 * *", "day of month"),
        ("0 0 * 13 *", "month"),
        ("0 0 * * 8", "day of week"),
    ] {
        let refusal = parse(expression).expect_err("refused");
        assert!(refusal.contains(field), "{expression}: {refusal}");
    }
}

#[test]
fn the_shapes_of_other_dialects_are_refused_rather_than_accepted() {
    // `?`, `L`, `W` and `#` belong to the Quartz family; a seconds field makes
    // it six. None of them is the dialect the type declared.
    for expression in ["0 0 ? * *", "0 0 L * *", "0 0 15W * *", "0 0 * * 6#3"] {
        assert!(parse(expression).is_err(), "{expression} should be refused");
    }
}

#[test]
fn a_malformed_range_or_step_is_refused_with_what_is_wrong() {
    for (expression, needle) in [
        ("17-5 * * * *", "backwards"),
        ("*/0 * * * *", "step of zero"),
        ("*/x * * * *", "not a number"),
        ("5/10 * * * *", "single value"),
        ("1- * * * *", "empty value"),
        ("- * * * *", "empty value"),
    ] {
        let refusal = parse(expression).expect_err("refused");
        assert!(refusal.contains(needle), "{expression}: {refusal}");
    }
}

#[test]
fn only_the_standard_dialect_is_one_this_gear_claims_to_check() {
    assert!(is_known("standard"));
    assert!(is_known("STANDARD"));
    assert!(!is_known("quartz"));
    assert!(!is_known("seconds"));
}
