//! Guard on how the anonymous read route is declared: this module registers
//! exactly one anonymous route, and it is a `GET`. The guards on the other
//! route files keep forbidding `.anonymous()` and `.public()`; this one pins
//! the single place that may use it.

/// The route declarations, read at compile time rather than from disk.
const SOURCE: &str = include_str!("public_routes.rs");

/// The declarations above the test module, so this file's own text, which
/// names the needles, is not counted.
fn declarations() -> &'static str {
    SOURCE
        .split("#[cfg(test)]")
        .next()
        .expect("the declarations precede the test module")
}

/// Count lines whose code, not prose, contains `needle`.
fn code_lines_matching(needle: &str) -> usize {
    declarations()
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("//"))
        .filter(|line| line.contains(needle))
        .count()
}

#[test]
fn exactly_one_route_is_declared_and_it_is_anonymous() {
    assert_eq!(code_lines_matching("OperationBuilder::"), 1);
    assert_eq!(code_lines_matching(".anonymous()"), 1);
    assert_eq!(code_lines_matching(".exposed()"), 1);
}

#[test]
fn the_anonymous_route_is_a_get_and_nothing_else() {
    assert_eq!(code_lines_matching("OperationBuilder::get("), 1);
    for method in ["post", "put", "patch", "delete", "head", "options"] {
        assert_eq!(
            code_lines_matching(&format!("OperationBuilder::{method}(")),
            0,
            "`{method}` on the anonymous surface"
        );
    }
}

#[test]
fn the_route_is_not_authenticated_and_does_not_use_the_retired_alias() {
    assert_eq!(code_lines_matching(".authenticated()"), 0);
    assert_eq!(code_lines_matching(".public()"), 0);
}

#[test]
fn no_rate_limit_is_declared_in_process() {
    for needle in ["require_rate_limit", "ThrottlingSpec", "rate_limit("] {
        assert_eq!(
            code_lines_matching(needle),
            0,
            "`{needle}`: throttling this route is the gateway's responsibility"
        );
    }
}
