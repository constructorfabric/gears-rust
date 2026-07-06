#[allow(unused_imports)]
use super::*;

use crate::domain::search::{MAX_PAGE_SIZE, SearchQuery};

#[test]
fn search_query_deserialises_from_json() {
    // Mirrors the JSON shape Axum lifts from `?q=needle&$top=10&$skip=5&context=2`.
    let q: SearchQuery =
        serde_json::from_str(r#"{"q":"needle","$top":10,"$skip":5,"context":2}"#).unwrap();
    assert_eq!(q.q.as_deref(), Some("needle"));
    assert_eq!(q.top, Some(10));
    assert_eq!(q.skip, Some(5));
    assert_eq!(q.context_radius, Some(2));
}

#[test]
fn search_query_clamps_top_to_max() {
    let q = SearchQuery {
        top: Some(9999),
        ..Default::default()
    };
    assert_eq!(q.effective_top(), MAX_PAGE_SIZE);
}

#[test]
fn search_query_default_top_when_zero() {
    let q = SearchQuery {
        top: Some(0),
        ..Default::default()
    };
    assert_eq!(q.effective_top(), 20);
}
