// @cpt-dod:cpt-cf-settings-service-dod-activation-read-tests:p1
//! The gate on how the activation read is declared, and on the regenerated
//! `OpenAPI` document that carries it.
//!
//! The declaration is read at compile time, as the sibling route guards read
//! theirs: `OperationBuilder`'s typestate makes a route choose an
//! authentication posture, not choose authentication, and nothing in the type
//! system says a route declares no metric of its own.

use serde_json::Value;

const SOURCE: &str = include_str!("activation_routes.rs");

/// The generated document the gear's routes are committed in.
const SPEC: &str = include_str!("../../../../../../docs/api/api.json");

/// The builder chain of the route, string continuations joined into one run.
fn declaration() -> String {
    let start = SOURCE
        .find("OperationBuilder::get(")
        .expect("the route is declared");
    let end = start
        + SOURCE[start..]
            .find(".register(")
            .expect("the builder is registered");
    SOURCE[start..end]
        .replace("\\\n", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn the_route_is_one_authenticated_get_with_no_licence_gate_and_its_registered_errors() {
    let route = declaration();
    assert!(
        route.contains("OperationBuilder::get(\"/settings-service/v1/settings/{key}/activation\")"),
        "{route}"
    );
    assert!(
        route.contains("\"settings_service.get_setting_activation\""),
        "{route}"
    );
    assert!(route.contains(".authenticated()"), "{route}");
    assert!(route.contains(".no_license_required()"), "{route}");
    for error in [
        ".error_400(",
        ".error_401(",
        ".error_403(",
        ".error_404(",
        ".error_500(",
        ".error_503(",
    ] {
        assert!(route.contains(error), "`{error}` is registered: {route}");
    }
    // `GET` only, authenticated only, and no metric of its own.
    let code: String = SOURCE
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let code = code.split("#[cfg(test)]").next().unwrap_or(&code);
    assert_eq!(code.matches("OperationBuilder::").count(), 1);
    for forbidden in [
        "OperationBuilder::put",
        "OperationBuilder::post",
        "OperationBuilder::patch",
        "OperationBuilder::delete",
        ".anonymous()",
        ".public()",
        "metric",
        "histogram",
        "counter",
    ] {
        assert!(
            !code.contains(forbidden),
            "`{forbidden}` in the declaration"
        );
    }
}

#[test]
fn the_description_tells_clients_to_treat_status_as_an_open_set() {
    let route = declaration();
    assert!(route.contains("treat it as an open set"), "{route}");
    assert!(
        ["awaiting", "succeeded", "failed"]
            .iter()
            .all(|status| route.contains(status)),
        "{route}"
    );
}

fn spec() -> Value {
    serde_json::from_str(SPEC).expect("the committed spec parses")
}

#[test]
fn the_regenerated_document_carries_the_route_under_its_operation_id() {
    let spec = spec();
    let path = "/settings-service/v1/settings/{key}/activation";
    let operations = spec["paths"][path]
        .as_object()
        .unwrap_or_else(|| panic!("`{path}` is in the document"));
    assert_eq!(operations.keys().collect::<Vec<_>>(), ["get"]);
    let get = &operations["get"];
    assert_eq!(
        get["operationId"],
        "settings_service.get_setting_activation"
    );
    for status in ["200", "400", "401", "403", "404", "500", "503"] {
        assert!(get["responses"].get(status).is_some(), "{status}");
    }
    assert!(
        get["description"]
            .as_str()
            .is_some_and(|d| d.contains("open set")),
        "{get}"
    );
    // The document carries no licence marker for any operation — a route that
    // requires none leaves no trace in it — so "no licence requirement" is
    // pinned at the source, by the route-declaration test above.
    let parameters: Vec<&str> = get["parameters"]
        .as_array()
        .expect("parameters")
        .iter()
        .filter_map(|p| p["name"].as_str())
        .collect();
    assert_eq!(parameters, ["key", "tenant"]);
}

#[test]
fn the_regenerated_document_carries_the_history_records_activations_change_ref_and_detail() {
    let spec = spec();
    let record = spec["components"]["schemas"]
        .as_object()
        .expect("schemas")
        .iter()
        .find(|(name, _)| name.ends_with("AuditRecordDto"))
        .map(|(_, schema)| schema)
        .expect("the history record schema");
    for property in ["activations", "change_ref", "detail"] {
        assert!(record["properties"].get(property).is_some(), "{property}");
    }
    assert!(
        record["required"]
            .as_array()
            .expect("required")
            .iter()
            .any(|name| name == "activations"),
        "every record carries `activations`"
    );
}
