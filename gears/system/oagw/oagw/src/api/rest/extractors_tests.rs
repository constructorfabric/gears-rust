use uuid::Uuid;

use super::parse_gts_id;
use crate::domain::gts_helpers as gts;

#[test]
fn parse_gts_id_accepts_an_id_of_the_expected_schema() {
    let id = Uuid::from_u128(42);

    let parsed = parse_gts_id(
        &gts::format_upstream_gts(id),
        gts::UPSTREAM_SCHEMA,
        "/oagw/v1/upstreams",
    )
    .expect("an upstream id parses as an upstream id");
    assert_eq!(parsed, id);
}

#[test]
fn parse_gts_id_rejects_an_id_of_another_schema() {
    let instance = "/oagw/v1/upstreams/some-id";

    let problem = parse_gts_id(
        &gts::format_route_gts(Uuid::from_u128(42)),
        gts::UPSTREAM_SCHEMA,
        instance,
    )
    .expect_err("a route id is not an upstream id");
    assert_eq!(problem.status, Some(400));
    assert_eq!(problem.instance.as_deref(), Some(instance));
}

/// The accepted list keys are exactly the query struct's fields: a key
/// listed but not read would be accepted and ignored, a field not listed
/// would be refused.
#[test]
fn accepted_list_keys_are_the_query_struct_fields() {
    use super::{ListQueryParams, PaginationQuery};
    use crate::api::rest::handlers::route::ListRoutesQuery;
    use axum::extract::Query;
    use serde::Serialize;

    fn check<T: ListQueryParams + Serialize + PartialEq + std::fmt::Debug>() {
        let keys = T::KEYS;
        let parse = |query: &str| -> T {
            let uri: axum::http::Uri = format!("http://h/?{query}").parse().unwrap();
            Query::<T>::try_from_uri(&uri).unwrap().0
        };
        let defaults = parse("");
        // Each listed key changes the struct on its own.
        for key in keys {
            assert_ne!(parse(&format!("{key}=7")), defaults, "{key} is not read");
        }
        // And the struct has no field the list leaves out.
        let mut fields: Vec<String> = serde_json::to_value(&defaults)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        fields.sort_unstable();
        let mut listed: Vec<&str> = keys.to_vec();
        listed.sort_unstable();
        assert_eq!(fields, listed);
    }
    check::<PaginationQuery>();
    check::<ListRoutesQuery>();
}

#[test]
fn unsupported_list_keys_are_capped_and_cut() {
    use super::{
        MAX_REPORTED_KEY_BYTES, MAX_REPORTED_QUERY_PARAMS, PaginationQuery,
        reject_unknown_query_params,
    };

    let long = "k".repeat(1000);
    let mut query: Vec<String> = (0..100).map(|i| format!("x{i}=1")).collect();
    query.insert(0, format!("{long}=1"));
    let problem =
        reject_unknown_query_params::<PaginationQuery>(&query.join("&"), "/i").unwrap_err();
    let body = serde_json::to_value(&*problem).unwrap();
    let fields: Vec<&str> = body["context"]["field_violations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["field"].as_str().unwrap())
        .collect();
    assert_eq!(fields.len(), MAX_REPORTED_QUERY_PARAMS);
    assert_eq!(fields[0], &long[..MAX_REPORTED_KEY_BYTES]);
    assert_eq!(fields[1], "x0");

    // A cut inside a multibyte character backs off to its start: 21 euro
    // signs (63 bytes) and the 64th byte inside the 22nd.
    let euros = "%E2%82%AC".repeat(30);
    let problem =
        reject_unknown_query_params::<PaginationQuery>(&format!("{euros}=1"), "/i").unwrap_err();
    let body = serde_json::to_value(&*problem).unwrap();
    assert_eq!(
        body["context"]["field_violations"][0]["field"],
        "\u{20ac}".repeat(21)
    );
}

proptest::proptest! {
    /// Whatever the query string, the check does not panic and reports at
    /// most the cap, each name within the byte limit.
    #[test]
    fn unsupported_list_keys_never_panic(query in "\\PC{0,400}") {
        use super::{
            MAX_REPORTED_KEY_BYTES, MAX_REPORTED_QUERY_PARAMS, PaginationQuery,
            reject_unknown_query_params,
        };
        if let Err(problem) = reject_unknown_query_params::<PaginationQuery>(&query, "/i") {
            let body = serde_json::to_value(&*problem).unwrap();
            let violations = body["context"]["field_violations"].as_array().unwrap().clone();
            proptest::prop_assert!(violations.len() <= MAX_REPORTED_QUERY_PARAMS);
            for v in violations {
                proptest::prop_assert!(v["field"].as_str().unwrap().len() <= MAX_REPORTED_KEY_BYTES);
            }
        }
    }
}
