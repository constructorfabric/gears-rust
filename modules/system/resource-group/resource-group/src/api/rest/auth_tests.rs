// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-testing-integration-auth:p1
use super::*;

fn default_config() -> MtlsConfig {
    MtlsConfig::default()
}

#[test]
fn jwt_mode_when_no_client_cn() {
    let config = default_config();
    let mode = determine_auth_mode(
        None,
        &http::Method::GET,
        "/api/resource-group/v1/groups/123/descendants",
        &config,
    );
    assert_eq!(mode, AuthMode::Jwt);
}

#[test]
fn mtls_mode_for_allowed_client_and_endpoint() {
    let config = default_config();
    let mode = determine_auth_mode(
        Some("authz-resolver-plugin"),
        &http::Method::GET,
        "/api/resource-group/v1/groups/some-uuid/descendants",
        &config,
    );
    assert_eq!(mode, AuthMode::Mtls);
}

#[test]
fn jwt_mode_for_unknown_client_cn() {
    let config = default_config();
    let mode = determine_auth_mode(
        Some("unknown-client"),
        &http::Method::GET,
        "/api/resource-group/v1/groups/some-uuid/descendants",
        &config,
    );
    assert_eq!(mode, AuthMode::Jwt);
}

#[test]
fn jwt_mode_for_disallowed_endpoint() {
    let config = default_config();
    let mode = determine_auth_mode(
        Some("authz-resolver-plugin"),
        &http::Method::POST,
        "/api/resource-group/v1/groups",
        &config,
    );
    assert_eq!(mode, AuthMode::Jwt);
}

#[test]
fn path_matching_with_param_segments() {
    assert!(path_matches_pattern(
        "/api/resource-group/v1/groups/abc-123/descendants",
        "/api/resource-group/v1/groups/{group_id}/descendants"
    ));
}

#[test]
fn path_matching_rejects_different_length() {
    assert!(!path_matches_pattern(
        "/api/resource-group/v1/groups",
        "/api/resource-group/v1/groups/{group_id}/descendants"
    ));
}

#[test]
fn path_matching_rejects_wrong_literal_segment() {
    assert!(!path_matches_pattern(
        "/api/resource-group/v1/groups/abc-123/members",
        "/api/resource-group/v1/groups/{group_id}/descendants"
    ));
}

// --- Phase 3: MTLS edge case tests ---

#[test]
fn mtls_mode_with_multiple_allowed_clients() {
    let config = MtlsConfig {
        allowed_clients: vec![
            "authz-resolver-plugin".to_owned(),
            "billing-service".to_owned(),
        ],
        ..MtlsConfig::default()
    };
    let mode = determine_auth_mode(
        Some("billing-service"),
        &http::Method::GET,
        "/api/resource-group/v1/groups/some-uuid/descendants",
        &config,
    );
    assert_eq!(mode, AuthMode::Mtls);
}

#[test]
fn mtls_rejected_for_put_to_hierarchy() {
    let config = default_config();
    let mode = determine_auth_mode(
        Some("authz-resolver-plugin"),
        &http::Method::PUT,
        "/api/resource-group/v1/groups/some-uuid/descendants",
        &config,
    );
    assert_eq!(
        mode,
        AuthMode::Jwt,
        "PUT to hierarchy should not be MTLS-allowed"
    );
}

#[test]
fn mtls_rejected_for_delete_to_groups() {
    let config = default_config();
    let mode = determine_auth_mode(
        Some("authz-resolver-plugin"),
        &http::Method::DELETE,
        "/api/resource-group/v1/groups/some-uuid",
        &config,
    );
    assert_eq!(
        mode,
        AuthMode::Jwt,
        "DELETE to groups should not be MTLS-allowed"
    );
}

#[test]
fn mtls_with_empty_client_cn() {
    let config = default_config();
    let mode = determine_auth_mode(
        Some(""),
        &http::Method::GET,
        "/api/resource-group/v1/groups/some-uuid/descendants",
        &config,
    );
    assert_eq!(mode, AuthMode::Jwt, "Empty CN should fall back to JWT");
}

#[test]
fn mtls_with_multiple_endpoints() {
    let config = MtlsConfig {
        allowed_endpoints: vec![
            AllowedEndpoint {
                method: http::Method::GET,
                path_pattern: "/api/resource-group/v1/groups/{group_id}/descendants".to_owned(),
            },
            AllowedEndpoint {
                method: http::Method::GET,
                path_pattern: "/api/resource-group/v1/types".to_owned(),
            },
        ],
        ..MtlsConfig::default()
    };

    assert_eq!(
        determine_auth_mode(
            Some("authz-resolver-plugin"),
            &http::Method::GET,
            "/api/resource-group/v1/groups/uuid/descendants",
            &config,
        ),
        AuthMode::Mtls,
    );
    assert_eq!(
        determine_auth_mode(
            Some("authz-resolver-plugin"),
            &http::Method::GET,
            "/api/resource-group/v1/types",
            &config,
        ),
        AuthMode::Mtls,
    );
    assert_eq!(
        determine_auth_mode(
            Some("authz-resolver-plugin"),
            &http::Method::POST,
            "/api/resource-group/v1/types",
            &config,
        ),
        AuthMode::Jwt,
    );
}
