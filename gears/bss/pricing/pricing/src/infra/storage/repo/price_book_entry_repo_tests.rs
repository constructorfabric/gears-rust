//! Rename L2: a storage failure of this repository names the entry table in its log line,
//! never the words the Price repository uses for `pricing_price`.
#![allow(clippy::expect_used, clippy::unwrap_used)]

/// Every context string handed to `driver_failure` or `map_unique` in this repository, and every
/// literal context a call of `distinct_skus` passes on to `driver_failure` (PT-02).
fn contexts(source: &str) -> Vec<&str> {
    let mut found = Vec::new();
    for call in ["driver_failure(\"", "map_unique(\""] {
        for (at, _) in source.match_indices(call) {
            let rest = &source[at + call.len()..];
            found.push(&rest[..rest.find('"').unwrap()]);
        }
    }
    for (at, _) in source.match_indices("distinct_skus(") {
        if source[..at].ends_with("fn ") {
            continue;
        }
        let call = &source[at..];
        let call = &call[..call.find(".await").unwrap()];
        let literals: Vec<&str> = call.split('"').skip(1).step_by(2).collect();
        assert_eq!(literals.len(), 1, "one literal context per call: {call}");
        found.extend(literals);
    }
    found
}
#[test]
fn every_storage_context_names_the_price_book_entry() {
    let found = contexts(include_str!("price_book_entry_repo.rs"));
    // Fourteen `driver_failure` and `map_unique` literals (one more with `find_many`, one with
    // `release_book`, D-522), and the three contexts `priced_skus`, `skus_in_book` (P-D-246) and
    // `in_plan_skus` pass to `distinct_skus`.
    assert_eq!(found.len(), 17, "{found:?}");
    for context in found {
        assert!(context.contains("price book entr"), "{context}");
    }
}
