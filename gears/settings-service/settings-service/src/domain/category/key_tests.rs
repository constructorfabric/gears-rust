// Created: 2026-08-13 by Constructor Tech
//! Tests for category key validation.
//!
//! Acceptance: FEATURE `category-management.md` §6 and CDSL
//! `cpt-cf-settings-service-algo-category-management-key-validation`.

use super::CategoryKey;
use crate::domain::error::DomainError;
use crate::field;

fn code_of(err: &DomainError) -> &'static str {
    match err {
        DomainError::Validation { code, .. } => code,
        other => panic!("expected a validation failure, got {other:?}"),
    }
}

#[test]
fn a_well_formed_key_is_accepted_verbatim() {
    let key = CategoryKey::parse("network").expect("valid");
    assert_eq!(key.as_str(), "network");
}

#[test]
fn a_key_is_never_trimmed_or_case_folded() {
    // Accepting " Network" as "network" would give one category two spellings,
    // and the setting keys declared under each would not match.
    for candidate in [" network", "network ", "Network", "NETWORK"] {
        let key = CategoryKey::parse(candidate).expect("valid characters");
        assert_eq!(
            key.as_str(),
            candidate,
            "`{candidate}` must be stored exactly as supplied"
        );
    }
}

#[test]
fn an_empty_key_is_refused() {
    let err = CategoryKey::parse("").expect_err("must be refused");
    assert_eq!(code_of(&err), field::CATEGORY_KEY_LENGTH);
}

#[test]
fn a_key_over_the_bound_is_refused() {
    let err = CategoryKey::parse(&"a".repeat(129)).expect_err("must be refused");
    assert_eq!(code_of(&err), field::CATEGORY_KEY_LENGTH);
}

#[test]
fn the_bound_is_inclusive_at_both_ends() {
    assert!(
        CategoryKey::parse("a").is_ok(),
        "1 character is the minimum"
    );
    assert!(
        CategoryKey::parse(&"a".repeat(128)).is_ok(),
        "128 characters is the maximum"
    );
}

#[test]
fn the_bound_counts_characters_not_bytes() {
    // A limit on what an administrator writes. Counting bytes would refuse a
    // 128-character key for a length its author never sees.
    let multibyte = "\u{e9}".repeat(128);
    assert!(multibyte.len() > 128, "the fixture is multi-byte");
    assert!(CategoryKey::parse(&multibyte).is_ok());
}

#[test]
fn a_key_containing_the_reserved_separator_is_refused() {
    // The key becomes the single category segment of every setting key declared
    // under it. A separator would suggest nesting the grammar cannot express.
    for candidate in ["net/work", "/network", "network/"] {
        let err = CategoryKey::parse(candidate).expect_err("must be refused");
        assert_eq!(
            code_of(&err),
            field::CATEGORY_KEY_RESERVED_SEPARATOR,
            "`{candidate}` must be refused for the separator"
        );
    }
}

#[test]
fn a_violation_names_the_field_a_caller_sent() {
    let err = CategoryKey::parse("").expect_err("must be refused");
    match err {
        DomainError::Validation { field, .. } => assert_eq!(field, super::FIELD),
        other => panic!("expected a validation failure, got {other:?}"),
    }
}

#[test]
fn the_length_rule_is_checked_before_the_separator_rule() {
    // An over-long key containing a separator violates both. Reporting length
    // first keeps the message about the bound the caller most likely hit.
    let err = CategoryKey::parse(&format!("{}/", "a".repeat(200))).expect_err("must be refused");
    assert_eq!(code_of(&err), field::CATEGORY_KEY_LENGTH);
}
