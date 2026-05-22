//! Claim mapping from Oidc claims into `SecurityContext`.
//!
//! This module contains the pure, synchronous mapping logic for Story 3
//! (claim mapping + security context normalization).

use authn_resolver_sdk::AuthNResolverError;
use modkit_macros::domain_model;
use modkit_security::SecurityContext;
use tracing::debug;
use uuid::Uuid;

use crate::domain::metrics::{
    AuthNMetrics, TOKEN_REJECTION_REASON_INVALID_TENANT, TOKEN_REJECTION_REASON_MISSING_TENANT,
};

/// Parsed claims map produced by JWT validation.
pub type Claims = serde_json::Map<String, serde_json::Value>;

/// Tenant identifier used by `SecurityContext.subject_tenant_id`.
pub type TenantId = Uuid;

/// Result type used by claim-mapping functions.
pub type Result<T> = std::result::Result<T, AuthNResolverError>;

#[domain_model]
#[derive(Debug, Clone)]
pub struct ClaimMapperConfig {
    pub subject_id: String,
    pub subject_tenant_id: String,
    pub subject_type: Option<String>,
    pub token_scopes: String,
}

#[domain_model]
#[derive(Debug, Clone, Default)]
pub struct ClaimMapperOptions {
    pub required_claims: Vec<String>,
    pub first_party_clients: Vec<String>,
}

#[must_use]
pub fn default_config() -> ClaimMapperConfig {
    ClaimMapperConfig {
        subject_id: "sub".to_owned(),
        subject_tenant_id: "tenant_id".to_owned(),
        subject_type: Some("user_type".to_owned()),
        token_scopes: "scope".to_owned(),
    }
}

/// Application type derived from claims and first-party client configuration.
#[domain_model]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum AppType {
    /// Trusted first-party app.
    FirstParty,
    /// External or unknown app.
    ThirdParty,
}

/// Extract and parse `sub` claim as UUID.
///
/// # Errors
/// Returns `Unauthorized("invalid subject id")` when the claim is absent,
/// not a string, or not a valid RFC 4122 UUID.
pub fn extract_subject_id(claims: &Claims) -> Result<Uuid> {
    extract_subject_id_with_claim(claims, "sub")
}

/// Extract and parse a configurable subject claim as UUID.
///
/// # Errors
/// Returns `Unauthorized("invalid subject id")` when the claim is absent,
/// not a string, or not a valid RFC 4122 UUID.
pub fn extract_subject_id_with_claim(claims: &Claims, claim_name: &str) -> Result<Uuid> {
    let subject = claims
        .get(claim_name)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| unauthorized("invalid subject id"))?;

    Uuid::parse_str(subject).map_err(|_| unauthorized("invalid subject id"))
}

/// Extract tenant UUID from a configurable claim.
///
/// # Errors
/// Returns `Unauthorized("missing claim")` when the configured claim is
/// absent or not a string.
/// Returns `Unauthorized("invalid tenant_id")` when the value is not a UUID.
pub fn extract_tenant_id(
    claims: &Claims,
    claim_name: &str,
    metrics: &AuthNMetrics,
) -> Result<TenantId> {
    let tenant_raw = claims
        .get(claim_name)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            metrics.increment_token_rejected(TOKEN_REJECTION_REASON_MISSING_TENANT);
            unauthorized_missing_claim(claim_name)
        })?;

    Uuid::parse_str(tenant_raw).map_err(|_| {
        metrics.increment_token_rejected(TOKEN_REJECTION_REASON_INVALID_TENANT);
        unauthorized("invalid tenant_id")
    })
}

/// Extract optional `user_type` claim.
#[must_use]
pub fn extract_user_type(claims: &Claims) -> Option<String> {
    extract_user_type_with_claim(claims, "user_type")
}

#[must_use]
pub fn extract_user_type_with_claim(claims: &Claims, claim_name: &str) -> Option<String> {
    claims
        .get(claim_name)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// Detect whether the calling application is first-party or third-party.
#[must_use]
pub fn detect_app_type(claims: &Claims, first_party_clients: &[String]) -> AppType {
    let client = claims
        .get("azp")
        .and_then(serde_json::Value::as_str)
        .or_else(|| claims.get("client_id").and_then(serde_json::Value::as_str));

    match client {
        Some(client_id) if first_party_clients.iter().any(|known| known == client_id) => {
            AppType::FirstParty
        }
        _ => AppType::ThirdParty,
    }
}

/// Extract token scopes based on application type.
#[must_use]
pub fn extract_scopes(claims: &Claims, app_type: AppType) -> Vec<String> {
    extract_scopes_with_claim(claims, app_type, "scope")
}

#[must_use]
pub fn extract_scopes_with_claim(
    claims: &Claims,
    app_type: AppType,
    claim_name: &str,
) -> Vec<String> {
    match app_type {
        AppType::FirstParty => vec!["*".to_owned()],
        AppType::ThirdParty => claims
            .get(claim_name)
            .and_then(serde_json::Value::as_str)
            .map(|scope| {
                scope
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect::<Vec<String>>()
            })
            .unwrap_or_default(),
    }
}

/// Map claims into a normalized `SecurityContext`.
///
/// # Errors
/// Returns `Unauthorized` for invalid/missing identity claims.
/// Returns `Internal` only if security-context construction fails unexpectedly.
pub fn map(
    claims: &Claims,
    config: &ClaimMapperConfig,
    metrics: &AuthNMetrics,
) -> Result<SecurityContext> {
    map_with_bearer(claims, config, None, metrics)
}

/// Map claims using shared mapper options.
///
/// # Errors
/// Returns `Unauthorized` for invalid/missing identity or required claims.
/// Returns `Internal` only if security-context construction fails unexpectedly.
pub fn map_with_options(
    claims: &Claims,
    config: &ClaimMapperConfig,
    options: &ClaimMapperOptions,
    metrics: &AuthNMetrics,
) -> Result<SecurityContext> {
    map_with_bearer_with_options(claims, config, options, None, metrics)
}

/// Map claims into a `SecurityContext`, optionally attaching the raw bearer token.
///
/// Building in one pass avoids re-constructing the context just to add the
/// bearer token, which would silently drop any future `SecurityContext` fields.
///
/// When `track_first_party` is `false` the first-party ratio gauge is not
/// updated — use this for S2S service-account tokens that would otherwise
/// skew the interactive-user ratio.
///
/// # Errors
/// Returns `Unauthorized` for invalid/missing identity claims.
/// Returns `Internal` only if security-context construction fails unexpectedly.
pub fn map_with_bearer(
    claims: &Claims,
    config: &ClaimMapperConfig,
    bearer_token: Option<&str>,
    metrics: &AuthNMetrics,
) -> Result<SecurityContext> {
    map_with_bearer_with_options(
        claims,
        config,
        &ClaimMapperOptions::default(),
        bearer_token,
        metrics,
    )
}

/// Map claims with a bearer token using shared mapper options.
///
/// # Errors
/// Returns `Unauthorized` for invalid/missing identity or required claims.
/// Returns `Internal` only if security-context construction fails unexpectedly.
pub fn map_with_bearer_with_options(
    claims: &Claims,
    config: &ClaimMapperConfig,
    options: &ClaimMapperOptions,
    bearer_token: Option<&str>,
    metrics: &AuthNMetrics,
) -> Result<SecurityContext> {
    map_with_bearer_opts(claims, config, options, bearer_token, metrics, true, None)
}

/// Internal mapping implementation with optional first-party tracking.
pub(crate) fn map_with_bearer_opts(
    claims: &Claims,
    config: &ClaimMapperConfig,
    options: &ClaimMapperOptions,
    bearer_token: Option<&str>,
    metrics: &AuthNMetrics,
    // False for S2S tokens: skip first-party ratio tracking and allow subject-type fallback.
    track_first_party: bool,
    default_subject_type: Option<&str>,
) -> Result<SecurityContext> {
    let subject_id = extract_subject_id_with_claim(claims, &config.subject_id)?;
    let subject_tenant_id = extract_tenant_id(claims, &config.subject_tenant_id, metrics)?;
    let mut subject_type = config
        .subject_type
        .as_deref()
        .and_then(|claim_name| extract_user_type_with_claim(claims, claim_name));
    for required_claim in &options.required_claims {
        if !claims.contains_key(required_claim) {
            return Err(unauthorized_missing_claim(required_claim));
        }
    }
    let app_type = detect_app_type(claims, &options.first_party_clients);
    if track_first_party {
        metrics.observe_first_party_auth(matches!(app_type, AppType::FirstParty));
    }
    let token_scopes = extract_scopes_with_claim(claims, app_type, &config.token_scopes);
    if !track_first_party && subject_type.is_none() {
        subject_type = default_subject_type.map(str::to_owned);
    }

    let mut builder = SecurityContext::builder()
        .subject_id(subject_id)
        .subject_tenant_id(subject_tenant_id)
        .token_scopes(token_scopes);

    if let Some(subject_type_value) = subject_type.as_deref() {
        builder = builder.subject_type(subject_type_value);
    }

    if let Some(token) = bearer_token {
        builder = builder.bearer_token(token.to_owned());
    }

    builder
        .build()
        .map_err(|e| AuthNResolverError::Internal(format!("failed to build security context: {e}")))
}

fn unauthorized(message: &str) -> AuthNResolverError {
    AuthNResolverError::Unauthorized(message.to_owned())
}

fn unauthorized_missing_claim(claim_name: &str) -> AuthNResolverError {
    debug!(claim = %claim_name, "OIDC token missing required claim");
    unauthorized("missing claim")
}

#[cfg(test)]
#[path = "claim_mapper_tests.rs"]
mod claim_mapper_tests;
