// @cpt-begin:cpt-cf-resource-group-dod-integration-auth-dual-auth:p1:inst-full
// @cpt-dod:cpt-cf-resource-group-dod-integration-auth-dual-auth:p1
//! Dual authentication mode routing for the resource-group module.
//!
//! JWT mode: standard `AuthN` -> `PolicyEnforcer` -> `AccessScope` pipeline.
//! MTLS mode: hierarchy-only bypass for `AuthZ` plugin (certificate verification
//! is handled by infrastructure/API gateway).

use std::path::PathBuf;

/// MTLS configuration for trusted system clients.
#[derive(Debug, Clone)]
pub struct MtlsConfig {
    /// Path to the trusted CA certificate bundle.
    pub ca_cert: PathBuf,
    /// Allowed client certificate CNs (e.g., "authz-resolver-plugin").
    pub allowed_clients: Vec<String>,
    /// Allowed method+path pairs for MTLS mode (e.g.,
    /// "GET /api/resource-group/v1/groups/{group_id}/hierarchy").
    pub allowed_endpoints: Vec<AllowedEndpoint>,
}

/// An endpoint allowed for MTLS access.
#[derive(Debug, Clone)]
pub struct AllowedEndpoint {
    pub method: http::Method,
    pub path_pattern: String,
}

/// Authentication mode determined per-request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthMode {
    /// Standard JWT authentication with full `AuthZ` evaluation.
    Jwt,
    /// MTLS authentication -- trusted system principal, `AuthZ` bypassed.
    Mtls,
}

// @cpt-algo:cpt-cf-resource-group-algo-integration-auth-auth-mode-decision:p1
/// Determine authentication mode from request context.
///
/// If the request has a valid client certificate header (forwarded by API gateway)
/// AND the endpoint is in the MTLS allowlist, returns [`AuthMode::Mtls`].
/// Otherwise returns [`AuthMode::Jwt`].
#[must_use]
pub fn determine_auth_mode(
    client_cn: Option<&str>,
    method: &http::Method,
    path: &str,
    config: &MtlsConfig,
) -> AuthMode {
    // @cpt-begin:cpt-cf-resource-group-algo-integration-auth-auth-mode-decision:p1:inst-auth-decide-1
    // Inspect request for authentication method
    // @cpt-end:cpt-cf-resource-group-algo-integration-auth-auth-mode-decision:p1:inst-auth-decide-1
    // @cpt-flow:cpt-cf-resource-group-flow-integration-auth-mtls-request:p1
    // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-mtls-request:p1:inst-mtls-1
    // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-mtls-request:p1:inst-mtls-2
    // AuthZ plugin sends request with MTLS client certificate
    // RG Gateway: extract client certificate from TLS handshake (handled by API gateway)
    // @cpt-end:cpt-cf-resource-group-flow-integration-auth-mtls-request:p1:inst-mtls-2
    // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-mtls-request:p1:inst-mtls-3
    // Validate certificate against trusted CA bundle (handled by API gateway)
    // @cpt-end:cpt-cf-resource-group-flow-integration-auth-mtls-request:p1:inst-mtls-3
    // @cpt-end:cpt-cf-resource-group-flow-integration-auth-mtls-request:p1:inst-mtls-1
    // @cpt-begin:cpt-cf-resource-group-algo-integration-auth-auth-mode-decision:p1:inst-auth-decide-2
    if let Some(cn) = client_cn
        // @cpt-begin:cpt-cf-resource-group-algo-integration-auth-auth-mode-decision:p1:inst-auth-decide-2a
        // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-mtls-request:p1:inst-mtls-4
        // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-mtls-request:p1:inst-mtls-5
        && config.allowed_clients.iter().any(|c| c == cn)
        // @cpt-end:cpt-cf-resource-group-flow-integration-auth-mtls-request:p1:inst-mtls-5
        // @cpt-end:cpt-cf-resource-group-flow-integration-auth-mtls-request:p1:inst-mtls-4
        // @cpt-end:cpt-cf-resource-group-algo-integration-auth-auth-mode-decision:p1:inst-auth-decide-2a
        // @cpt-begin:cpt-cf-resource-group-algo-integration-auth-auth-mode-decision:p1:inst-auth-decide-2b
        // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-mtls-request:p1:inst-mtls-6
        // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-mtls-request:p1:inst-mtls-7
        && is_endpoint_allowed(method, path, &config.allowed_endpoints)
    // @cpt-end:cpt-cf-resource-group-flow-integration-auth-mtls-request:p1:inst-mtls-7
    // @cpt-end:cpt-cf-resource-group-flow-integration-auth-mtls-request:p1:inst-mtls-6
    // @cpt-end:cpt-cf-resource-group-algo-integration-auth-auth-mode-decision:p1:inst-auth-decide-2b
    {
        // @cpt-begin:cpt-cf-resource-group-algo-integration-auth-auth-mode-decision:p1:inst-auth-decide-2c
        // Check endpoint in MTLS allowlist (covered by is_endpoint_allowed above)
        // @cpt-end:cpt-cf-resource-group-algo-integration-auth-auth-mode-decision:p1:inst-auth-decide-2c
        // @cpt-begin:cpt-cf-resource-group-algo-integration-auth-auth-mode-decision:p1:inst-auth-decide-2d
        // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-mtls-request:p1:inst-mtls-8
        // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-mtls-request:p1:inst-mtls-9
        // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-mtls-request:p1:inst-mtls-10
        // All checks pass -> create system SecurityContext, skip AuthZ -> RETURN MTLS mode
        return AuthMode::Mtls;
        // @cpt-end:cpt-cf-resource-group-flow-integration-auth-mtls-request:p1:inst-mtls-10
        // @cpt-end:cpt-cf-resource-group-flow-integration-auth-mtls-request:p1:inst-mtls-9
        // @cpt-end:cpt-cf-resource-group-flow-integration-auth-mtls-request:p1:inst-mtls-8
        // @cpt-end:cpt-cf-resource-group-algo-integration-auth-auth-mode-decision:p1:inst-auth-decide-2d
    }
    // @cpt-begin:cpt-cf-resource-group-algo-integration-auth-auth-mode-decision:p1:inst-auth-decide-2e
    // ELSE: MTLS checks failed -> fall through to JWT (403 handled by middleware)
    // @cpt-end:cpt-cf-resource-group-algo-integration-auth-auth-mode-decision:p1:inst-auth-decide-2e
    // @cpt-end:cpt-cf-resource-group-algo-integration-auth-auth-mode-decision:p1:inst-auth-decide-2
    // @cpt-begin:cpt-cf-resource-group-algo-integration-auth-auth-mode-decision:p1:inst-auth-decide-3
    // IF request has JWT bearer token (JWT authentication handled by middleware)
    // @cpt-begin:cpt-cf-resource-group-algo-integration-auth-auth-mode-decision:p1:inst-auth-decide-3a
    // Authenticate via AuthNResolverClient -> SecurityContext (handled by middleware)
    // @cpt-end:cpt-cf-resource-group-algo-integration-auth-auth-mode-decision:p1:inst-auth-decide-3a
    // @cpt-begin:cpt-cf-resource-group-algo-integration-auth-auth-mode-decision:p1:inst-auth-decide-3b
    // Run PolicyEnforcer.access_scope() -> AccessScope (handled by middleware)
    // @cpt-end:cpt-cf-resource-group-algo-integration-auth-auth-mode-decision:p1:inst-auth-decide-3b
    // @cpt-begin:cpt-cf-resource-group-algo-integration-auth-auth-mode-decision:p1:inst-auth-decide-3c
    // RETURN JWT mode with SecurityContext + AccessScope
    // @cpt-end:cpt-cf-resource-group-algo-integration-auth-auth-mode-decision:p1:inst-auth-decide-3c
    // @cpt-end:cpt-cf-resource-group-algo-integration-auth-auth-mode-decision:p1:inst-auth-decide-3
    // @cpt-flow:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1
    // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-1
    // Actor sends request to any RG REST endpoint with JWT bearer token
    // @cpt-end:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-1
    // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-2
    // API Gateway: authenticate JWT via AuthNResolverClient -> SecurityContext (handled by middleware)
    // @cpt-end:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-2
    // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-3
    // RG Gateway: call PolicyEnforcer.access_scope(ctx, resource_type, action)
    // @cpt-end:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-3
    // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-4
    // PolicyEnforcer -> AuthZ Resolver: evaluate(EvaluationRequest)
    // @cpt-end:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-4
    // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-5
    // AuthZ plugin internally: call ResourceGroupReadHierarchy.list_group_depth() (via MTLS/in-process)
    // @cpt-end:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-5
    // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-6
    // AuthZ plugin: produce constraints (e.g., owner_tenant_id IN (...))
    // @cpt-end:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-6
    // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-7
    // PolicyEnforcer: compile_to_access_scope() -> AccessScope
    // @cpt-end:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-7
    // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-8
    // RG Gateway: apply AccessScope via SecureORM (WHERE tenant_id IN (...))
    // @cpt-end:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-8
    // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-9
    // RG Service: execute query with SQL predicates, return results
    // @cpt-end:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-9
    // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-10
    // RETURN response to actor
    // @cpt-end:cpt-cf-resource-group-flow-integration-auth-jwt-request:p1:inst-jwt-10
    // @cpt-begin:cpt-cf-resource-group-algo-integration-auth-auth-mode-decision:p1:inst-auth-decide-4
    // ELSE -> RETURN 401 Unauthorized (fallback to JWT mode; 401 handled by middleware)
    AuthMode::Jwt
    // @cpt-end:cpt-cf-resource-group-algo-integration-auth-auth-mode-decision:p1:inst-auth-decide-4
}

/// Check if the given method+path matches any allowed endpoint pattern.
fn is_endpoint_allowed(method: &http::Method, path: &str, endpoints: &[AllowedEndpoint]) -> bool {
    endpoints
        .iter()
        .any(|ep| ep.method == *method && path_matches_pattern(path, &ep.path_pattern))
}

/// Simple path pattern matching supporting `{param}` segments.
fn path_matches_pattern(path: &str, pattern: &str) -> bool {
    let path_segments: Vec<&str> = path.split('/').collect();
    let pattern_segments: Vec<&str> = pattern.split('/').collect();

    if path_segments.len() != pattern_segments.len() {
        return false;
    }

    path_segments
        .iter()
        .zip(pattern_segments.iter())
        .all(|(p, pat)| (pat.starts_with('{') && pat.ends_with('}')) || p == pat)
}

impl Default for MtlsConfig {
    fn default() -> Self {
        Self {
            ca_cert: PathBuf::from("/etc/ssl/certs/rg-mtls-ca.pem"),
            allowed_clients: vec!["authz-resolver-plugin".to_owned()],
            allowed_endpoints: vec![AllowedEndpoint {
                method: http::Method::GET,
                path_pattern: "/api/resource-group/v1/groups/{group_id}/hierarchy".to_owned(),
            }],
        }
    }
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
// @cpt-end:cpt-cf-resource-group-dod-integration-auth-dual-auth:p1:inst-full
