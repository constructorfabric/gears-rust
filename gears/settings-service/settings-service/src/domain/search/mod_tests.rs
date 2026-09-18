// Created: 2026-09-17 by Constructor Tech
//! The search vocabulary: what a query may be, how it becomes a pattern, and
//! how a value is read as text.

use serde_json::json;
use uuid::Uuid;

use super::{
    Corpus, MAX_QUERY_CHARS, MatchedField, Needle, cursor_binding, like_escape, text_projection,
};
use crate::domain::error::DomainError;
use crate::field;

fn refusal(raw: &str) -> DomainError {
    Needle::parse(raw).expect_err("refused")
}

#[test]
fn a_query_is_trimmed_and_must_keep_at_least_two_characters() {
    assert_eq!(Needle::parse("  ab  ").expect("accepted").as_str(), "ab");
    for raw in ["", " ", "a", " a "] {
        match refusal(raw) {
            DomainError::Validation { field, code, .. } => {
                assert_eq!(field, "q", "{raw:?}");
                assert_eq!(code, field::SEARCH_QUERY, "{raw:?}");
            }
            other => panic!("{raw:?}: expected a validation refusal, got {other:?}"),
        }
    }
}

#[test]
fn a_query_longer_than_the_bound_is_refused_and_one_at_the_bound_is_not() {
    let at_bound = "x".repeat(MAX_QUERY_CHARS);
    assert!(Needle::parse(&at_bound).is_ok());
    let over = "x".repeat(MAX_QUERY_CHARS + 1);
    assert!(matches!(refusal(&over), DomainError::Validation { .. }));
}

#[test]
fn the_pattern_wraps_the_needle_and_escapes_the_wildcards_and_the_escape_char() {
    assert_eq!(like_escape("100%"), r"100\%");
    assert_eq!(like_escape("a_b"), r"a\_b");
    assert_eq!(like_escape(r"c:\dir"), r"c:\\dir");
    assert_eq!(like_escape("plain"), "plain");
    assert_eq!(
        Needle::parse("50%_x").expect("accepted").like_pattern(),
        r"%50\%\_x%"
    );
}

#[test]
fn matching_in_rust_is_case_insensitive_and_a_substring_test() {
    let needle = Needle::parse("Proxy").expect("accepted");
    assert!(needle.matches("enable_proxy"));
    assert!(needle.matches("PROXY settings"));
    assert!(!needle.matches("prox"));
}

#[test]
fn the_corpus_is_public_alone_without_the_entitlement_and_never_secret() {
    assert_eq!(Corpus::for_caller(false), Corpus::Public);
    assert_eq!(Corpus::for_caller(true), Corpus::PublicAndPii);
    assert_eq!(Corpus::Public.classifications(), &["public"]);
    assert_eq!(Corpus::PublicAndPii.classifications(), &["public", "pii"]);
    for corpus in [Corpus::Public, Corpus::PublicAndPii] {
        assert!(!corpus.admits("secret"), "{corpus:?}");
    }
    assert!(!Corpus::Public.admits("pii"));
    assert!(Corpus::PublicAndPii.admits("pii"));
}

#[test]
fn the_matched_field_vocabulary_is_the_contracts_in_the_order_a_client_is_told() {
    let all = [
        MatchedField::Key,
        MatchedField::Description,
        MatchedField::CategoryName,
        MatchedField::DefaultValue,
        MatchedField::Value,
    ];
    assert_eq!(
        all.iter().map(|f| f.as_str()).collect::<Vec<_>>(),
        [
            "key",
            "description",
            "category_name",
            "default_value",
            "value"
        ]
    );
    assert!(MatchedField::Key < MatchedField::Description);
    assert!(MatchedField::DefaultValue < MatchedField::Value);
}

#[test]
fn a_string_projects_as_itself_and_anything_else_as_its_json_text() {
    assert_eq!(text_projection(&json!("hunter")), "hunter");
    assert_eq!(text_projection(&json!(90)), "90");
    assert_eq!(text_projection(&json!(true)), "true");
    assert_eq!(text_projection(&json!({"a": 1})), r#"{"a":1}"#);
    assert_eq!(text_projection(&json!(null)), "null");
}

#[test]
fn the_cursor_binding_changes_with_the_query_the_target_and_the_corpus_and_only_those() {
    let needle = Needle::parse("proxy").expect("accepted");
    let other = Needle::parse("proxz").expect("accepted");
    let tenant = Uuid::from_u128(1);
    let base = cursor_binding(&needle, tenant, Corpus::Public);
    assert_eq!(
        base,
        cursor_binding(&needle, tenant, Corpus::Public),
        "stable"
    );
    assert_ne!(base, cursor_binding(&other, tenant, Corpus::Public));
    assert_ne!(
        base,
        cursor_binding(&needle, Uuid::from_u128(2), Corpus::Public)
    );
    assert_ne!(base, cursor_binding(&needle, tenant, Corpus::PublicAndPii));
    assert_eq!(base.len(), 16, "a fixed-width hex token: {base}");
}
