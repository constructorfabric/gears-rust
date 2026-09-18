// Created: 2026-09-17 by Constructor Tech
//! Guards on how the search route is declared: authenticated, and never
//! opting out — a search served without a principal would expose the
//! catalogue and, through its counts, the values in it.

/// The route declarations, read at compile time rather than from disk.
const SOURCE: &str = include_str!("search_routes.rs");

/// Count lines whose code -- not prose -- contains `needle`.
fn code_lines_matching(needle: &str) -> usize {
    SOURCE
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("//"))
        .filter(|line| line.contains(needle))
        .count()
}

#[test]
fn the_search_route_requires_an_authenticated_principal() {
    let routes = code_lines_matching("OperationBuilder::");
    let authenticated = code_lines_matching(".authenticated()");
    assert!(routes > 0, "the fixture must find the route declaration");
    assert_eq!(authenticated, routes);
}

#[test]
fn the_search_route_does_not_opt_out_of_authentication() {
    for opt_out in [".anonymous()", ".public()"] {
        assert_eq!(
            code_lines_matching(opt_out),
            0,
            "`{opt_out}` on a search route"
        );
    }
}

#[test]
fn the_route_documents_the_closed_matched_field_vocabulary_and_the_bounds() {
    for phrase in [
        "`key`, `description`, `category_name`, `default_value` or `value`",
        "two characters",
        "two hundred",
    ] {
        assert!(
            SOURCE.contains(phrase),
            "the description must say: {phrase}"
        );
    }
}
