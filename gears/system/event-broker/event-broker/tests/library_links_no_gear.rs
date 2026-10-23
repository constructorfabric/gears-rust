//! The library links no gear but types-registry: a gear crate in the dependency
//! graph registers itself (`inventory`) in every binary that links the broker.
//! A test binary cannot observe what a library build links, so the manifest
//! itself is checked: the library, `test-utils` (which other crates' tests
//! enable) and the dev-dependencies.

#![allow(clippy::expect_used)]

/// Gear crates only the standalone server may link.
const SERVER_ONLY: &[&str] = &[
    "cluster",
    "authz-resolver",
    "api-gateway",
    "grpc_hub",
    "authn-resolver",
    "static-authn-plugin",
    "static-authz-plugin",
];

/// The one feature allowed to enable them; it is not on by default.
const ALLOWED_FEATURES: &[&str] = &["server"];

fn manifest() -> toml::Table {
    include_str!("../Cargo.toml")
        .parse()
        .expect("Cargo.toml parses")
}

#[test]
fn server_only_gears_are_optional_dependencies() {
    let manifest = manifest();
    let deps = manifest["dependencies"]
        .as_table()
        .expect("[dependencies] is a table");
    for name in SERVER_ONLY {
        let dep = deps
            .get(*name)
            .unwrap_or_else(|| panic!("`{name}` is no longer a dependency; update SERVER_ONLY"));
        let optional = dep
            .get("optional")
            .and_then(toml::Value::as_bool)
            .unwrap_or(false);
        assert!(optional, "`{name}` must stay optional, behind `server`");
    }
}

#[test]
fn only_server_enables_them() {
    let manifest = manifest();
    let features = manifest["features"]
        .as_table()
        .expect("[features] is a table");
    if let Some(default) = features.get("default") {
        assert!(
            default.as_array().is_some_and(Vec::is_empty),
            "a default feature would link gears into every consumer: {default}"
        );
    }
    for (feature, enables) in features {
        if ALLOWED_FEATURES.contains(&feature.as_str()) {
            continue;
        }
        for entry in enables.as_array().expect("a feature is a list") {
            let entry = entry.as_str().expect("a feature entry is a string");
            let dep = entry.strip_prefix("dep:").unwrap_or(entry);
            let dep = dep.split('/').next().unwrap_or(dep);
            assert!(
                !SERVER_ONLY.contains(&dep),
                "feature `{feature}` enables `{entry}`, which links a gear into the library"
            );
        }
    }
}

#[test]
fn tests_reach_the_gears_only_through_server() {
    let manifest = manifest();
    let dev = manifest["dev-dependencies"]
        .as_table()
        .expect("[dev-dependencies] is a table");
    for name in SERVER_ONLY {
        assert!(
            !dev.contains_key(*name),
            "`{name}` is a dev-dependency; a test that needs the gear goes behind `server`"
        );
    }
}
