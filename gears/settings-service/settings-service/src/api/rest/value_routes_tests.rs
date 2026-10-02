// Created: 2026-09-25 by Virtuozzo International GmbH
//! Guards on how the value routes are declared.

/// The route declarations, read at compile time rather than from disk.
const SOURCE: &str = include_str!("value_routes.rs");

/// Each route's declaration: its path and the builder chain that follows it.
fn routes() -> Vec<(&'static str, &'static str)> {
    SOURCE
        .split("OperationBuilder::")
        .skip(1)
        .map(|chunk| {
            let path = chunk
                .split('"')
                .nth(1)
                .expect("every route names its path first");
            (path, chunk)
        })
        .collect()
}

#[test]
fn every_route_whose_handler_meets_a_retired_declaration_declares_410() {
    // Each of these gates on the declaration, and a retired one answers
    // `410` — at the gate, or again at commit for one retired in between. A
    // batch answers per entry, so its route carries no `410` of its own.
    let routes = routes();
    assert!(
        routes.len() >= 8,
        "the fixture must find the routes: {}",
        routes.len()
    );
    for (path, chain) in routes {
        if path.ends_with("/settings/batch") {
            continue;
        }
        assert!(
            chain.contains("StatusCode::GONE"),
            "`{path}` can answer 410 but does not declare it"
        );
    }
}
