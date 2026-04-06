use serde::Deserialize;
use url::Url;

use super::error::TokenError;

/// Minimal subset of the `OpenID` Connect discovery document.
///
/// Only `token_endpoint` is required; all other fields are silently ignored.
#[derive(Deserialize)]
struct OidcDiscoveryDoc {
    token_endpoint: String,
}

/// Resolve the token endpoint from an OIDC issuer URL.
///
/// Fetches `{issuer_url}/.well-known/openid-configuration` and extracts
/// the `token_endpoint` field. This is a one-time operation at startup.
///
/// # Errors
///
/// Returns [`TokenError::Http`] if the discovery request fails or returns a
/// non-success status.
/// Returns [`TokenError::InvalidResponse`] if the response body cannot be
/// parsed, the `token_endpoint` field is missing, or it is not a valid URL.
pub async fn discover_token_endpoint(
    client: &modkit_http::HttpClient,
    issuer_url: &Url,
) -> Result<Url, TokenError> {
    let base = issuer_url.as_str().trim_end_matches('/');
    let discovery_url = format!("{base}/.well-known/openid-configuration");

    let doc: OidcDiscoveryDoc = client
        .get(&discovery_url)
        .send()
        .await
        .map_err(|e| TokenError::Http(crate::http_error::format_http_error(&e, "OIDC discovery")))?
        .error_for_status()
        .map_err(|e| TokenError::Http(crate::http_error::format_http_error(&e, "OIDC discovery")))?
        .json()
        .await
        .map_err(|e| {
            TokenError::InvalidResponse(crate::http_error::format_http_error(&e, "OIDC discovery"))
        })?;

    Url::parse(&doc.token_endpoint).map_err(|e| {
        TokenError::InvalidResponse(format!(
            "invalid token_endpoint URL in discovery document: {e}"
        ))
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "discovery_tests.rs"]
mod tests;
