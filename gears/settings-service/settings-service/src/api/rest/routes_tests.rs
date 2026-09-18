// Created: 2026-08-25 by Constructor Tech
//! Guards on how the category routes are declared.
//!
//! `OperationBuilder`'s typestate forces every route to *choose* an
//! authentication posture before it can register -- `authenticated`,
//! `anonymous` and `public` all satisfy `AuthSet`. It does not force the choice
//! to be authentication. A settings route served anonymously would expose the
//! platform's configuration surface, and nothing in the type system would say
//! so, which is why the choice is asserted here.

/// The route declarations, read at compile time rather than from disk.
const SOURCE: &str = include_str!("routes.rs");

/// Count lines whose code -- not prose -- is exactly `needle`.
///
/// Prose is excluded deliberately: the module doc mentions `.authenticated()`
/// when explaining why routes are built through `OperationBuilder`, and a
/// substring count would read that sentence as a sixth route.
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
    // The two escape hatches, named explicitly so adding one is a deliberate
    // act that fails this test rather than a quiet edit nobody reviews.
    for opt_out in [".anonymous()", ".public()"] {
        assert!(
            code_lines_matching(opt_out) == 0,
            "`{opt_out}` would serve a settings route without a principal"
        );
    }
}
