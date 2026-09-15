// Created: 2026-08-26 by Constructor Tech
//! Guards on how the declaration routes are declared.
//!
//! The same rule the category routes carry, asserted separately because
//! `OperationBuilder`'s typestate forces each route to *choose* a posture and
//! `anonymous` and `public` satisfy it just as `authenticated` does. A
//! declaration listing served without a principal would expose the platform's
//! configuration catalogue.

/// The route declarations, read at compile time rather than from disk.
const SOURCE: &str = include_str!("declaration_routes.rs");

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
fn every_route_requires_an_authenticated_principal() {
    let routes = code_lines_matching("OperationBuilder::");
    let authenticated = code_lines_matching(".authenticated()");
    assert!(routes > 0, "the fixture must find the route declarations");
    assert_eq!(
        authenticated, routes,
        "every route must call `.authenticated()`; {routes} routes declared, \
         {authenticated} authenticated"
    );
}

#[test]
fn no_route_opts_out_of_authentication() {
    for opt_out in [".anonymous()", ".public()"] {
        assert!(
            code_lines_matching(opt_out) == 0,
            "`{opt_out}` would serve a declaration route without a principal"
        );
    }
}
