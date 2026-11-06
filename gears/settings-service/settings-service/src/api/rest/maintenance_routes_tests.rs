//! Guards on how the maintenance routes are declared: authenticated, never
//! anonymous, and never published through the gateway.

/// The route declarations, read at compile time rather than from disk.
const SOURCE: &str = include_str!("maintenance_routes.rs");

/// The declarations above the test module.
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
fn every_maintenance_route_requires_an_authenticated_principal() {
    let routes = code_lines_matching("OperationBuilder::");
    assert_eq!(routes, 2, "the sweep and the prune");
    assert_eq!(code_lines_matching(".authenticated()"), routes);
}

#[test]
fn no_maintenance_route_opts_out_of_authentication_or_is_exposed() {
    for needle in [".anonymous()", ".public()", ".exposed()"] {
        assert_eq!(
            code_lines_matching(needle),
            0,
            "`{needle}` on an internal route"
        );
    }
}

#[test]
fn both_routes_are_posts() {
    assert_eq!(code_lines_matching("OperationBuilder::post("), 2);
}
