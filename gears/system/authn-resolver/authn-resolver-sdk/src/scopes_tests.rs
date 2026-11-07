use super::*;

fn set(scopes: &[&str]) -> ScopeSet {
    ScopeSet::new(scopes).unwrap()
}

#[test]
fn canonical_form_is_sorted_deduplicated_and_case_sensitive() {
    let scopes = set(&["write", "Read", "read", "write"]);
    assert_eq!(scopes.as_slice(), ["Read", "read", "write"]);
    assert_eq!(scopes.to_string(), "Read read write");
    assert_eq!(scopes.iter().collect::<Vec<_>>(), ["Read", "read", "write"]);
}

#[test]
fn order_and_duplicates_do_not_change_identity() {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let hash = |scopes: &ScopeSet| {
        let mut hasher = DefaultHasher::new();
        scopes.hash(&mut hasher);
        hasher.finish()
    };
    let a = set(&["b", "a", "b"]);
    let b = set(&["a", "b"]);
    assert_eq!(a, b);
    assert_eq!(hash(&a), hash(&b));
    assert_ne!(a, set(&["a"]), "no subset identity");
    assert_ne!(set(&["A"]), set(&["a"]));
}

#[test]
fn every_rfc_6749_scope_token_byte_is_accepted() {
    let all: String = (0x21u8..=0x7E)
        .filter(|b| *b != b'"' && *b != b'\\')
        .map(char::from)
        .collect();
    assert_eq!(all.len(), 92);
    assert_eq!(set(&[all.as_str()]).as_slice(), [all.as_str()]);
}

#[test]
fn bytes_outside_the_scope_token_grammar_are_rejected_without_echo() {
    for bad in [
        "", " ", "a b", " a", "a ", "\t", "a\"b", "a\\b", "\u{7f}", "\u{e9}", "a\nb",
    ] {
        let error = ScopeSet::new(["ok", bad]).unwrap_err();
        assert_eq!(error, ScopeSetError::InvalidToken { index: 1 }, "{bad:?}");
    }
    let error = ScopeSet::new(["secret value"]).unwrap_err();
    assert!(!error.to_string().contains("secret"), "{error}");
}

#[test]
fn an_empty_set_is_rejected() {
    assert_eq!(
        ScopeSet::new(Vec::<String>::new()).unwrap_err(),
        ScopeSetError::Empty
    );
}

#[test]
fn raw_count_is_bounded_before_deduplication() {
    let at_limit = vec!["a"; ScopeSet::MAX_SCOPES];
    assert_eq!(set(&at_limit).as_slice(), ["a"]);
    let over = vec!["a"; ScopeSet::MAX_SCOPES + 1];
    assert_eq!(ScopeSet::new(over).unwrap_err(), ScopeSetError::TooMany);
}

#[test]
fn pathological_input_stops_at_the_first_excess_item() {
    // An endless iterator proves the bound is enforced while iterating.
    let endless = std::iter::repeat("a");
    assert_eq!(ScopeSet::new(endless).unwrap_err(), ScopeSetError::TooMany);
    let endless_blank = std::iter::repeat("");
    assert_eq!(
        ScopeSet::new(endless_blank).unwrap_err(),
        ScopeSetError::InvalidToken { index: 0 }
    );
}

#[test]
fn token_length_is_bounded() {
    let longest = "a".repeat(ScopeSet::MAX_SCOPE_LEN);
    assert!(ScopeSet::new([longest.as_str()]).is_ok());
    let too_long = "a".repeat(ScopeSet::MAX_SCOPE_LEN + 1);
    assert_eq!(
        ScopeSet::new([too_long.as_str()]).unwrap_err(),
        ScopeSetError::TokenTooLong { index: 0 }
    );
}

#[test]
fn aggregate_raw_size_is_bounded_including_duplicates() {
    let token = "a".repeat(ScopeSet::MAX_SCOPE_LEN);
    let fits = 16;
    assert_eq!(fits * ScopeSet::MAX_SCOPE_LEN, ScopeSet::MAX_TOTAL_LEN);
    assert!(ScopeSet::new(vec![token.as_str(); fits]).is_ok());
    assert_eq!(
        ScopeSet::new(vec![token.as_str(); fits + 1]).unwrap_err(),
        ScopeSetError::TooLarge
    );
}

#[test]
fn optional_set_treats_no_scopes_as_idp_defaults() {
    assert_eq!(ScopeSet::optional(Vec::<String>::new()).unwrap(), None);
    assert_eq!(
        ScopeSet::optional(["b", "a"]).unwrap(),
        Some(set(&["a", "b"]))
    );
    assert_eq!(
        ScopeSet::optional([""]).unwrap_err(),
        ScopeSetError::InvalidToken { index: 0 }
    );
}

#[test]
fn client_credentials_requests_default_to_idp_scopes_and_take_a_scope_set() {
    use crate::models::ClientCredentialsRequest;
    let request = ClientCredentialsRequest::new("svc", secrecy::SecretString::from("secret"));
    assert_eq!(request.scopes, None, "IdP defaults");

    let request = request.with_scopes(set(&["write", "read"]));
    assert_eq!(request.scopes, Some(set(&["read", "write"])));
}
