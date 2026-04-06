use super::*;

#[test]
fn derive_simple_path() {
    let (prefix, mode) = derive_route_match("/v1/responses");
    assert_eq!(prefix, "/v1/responses");
    assert!(matches!(mode, oagw_sdk::PathSuffixMode::Disabled));
}

#[test]
fn derive_path_with_model_placeholder() {
    let (prefix, mode) =
        derive_route_match("/openai/deployments/{model}/responses?api-version=2025-03-01");
    assert_eq!(prefix, "/openai/deployments");
    assert!(matches!(mode, oagw_sdk::PathSuffixMode::Append));
}

#[test]
fn derive_azure_openai_path() {
    let (prefix, mode) = derive_route_match("/openai/v1/responses");
    assert_eq!(prefix, "/openai/v1/responses");
    assert!(matches!(mode, oagw_sdk::PathSuffixMode::Disabled));
}

#[test]
fn extract_empty_query() {
    assert!(extract_query_allowlist("/v1/responses").is_empty());
}

#[test]
fn extract_single_query_param() {
    let params =
        extract_query_allowlist("/openai/deployments/{model}/responses?api-version=2025-03-01");
    assert_eq!(params, vec!["api-version"]);
}

#[test]
fn extract_multiple_query_params() {
    let params = extract_query_allowlist("/path?foo=1&bar=2&baz=3");
    assert_eq!(params, vec!["foo", "bar", "baz"]);
}

#[test]
fn derive_trailing_wildcard_strips_trailing_slash() {
    let (prefix, mode) = derive_route_match("/v1/models/*/completions");
    assert_eq!(prefix, "/v1/models");
    assert!(matches!(mode, oagw_sdk::PathSuffixMode::Append));
}

#[test]
fn derive_root_path() {
    let (prefix, mode) = derive_route_match("/");
    assert_eq!(prefix, "/");
    assert!(matches!(mode, oagw_sdk::PathSuffixMode::Disabled));
}

#[test]
fn derive_query_string_stripped_before_matching() {
    // Query params should not affect route prefix or suffix mode.
    let (prefix, mode) = derive_route_match("/v1/responses?stream=true");
    assert_eq!(prefix, "/v1/responses");
    assert!(matches!(mode, oagw_sdk::PathSuffixMode::Disabled));
}

#[test]
fn extract_query_params_with_empty_values() {
    let params = extract_query_allowlist("/path?key=&other=val");
    assert_eq!(params, vec!["key", "other"]);
}
