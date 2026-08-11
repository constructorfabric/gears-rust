//! Public REST handlers for the token-issuer: JWKS + OIDC discovery, plus the
//! gated OBO re-mint endpoint.

use std::sync::Arc;

use axum::extract::Extension;
use toolkit::api::canonical_prelude::*;

use crate::api::rest::dto::{RemintRequest, RemintResponse};
use crate::domain::error::DomainError;
use crate::domain::peer_identity::PeerConnInfo;
use crate::domain::service::Service;

/// `GET /issuers/cap/jwks.json` — capability-token JWKS.
///
/// # Errors
/// Returns a canonical `Problem` (503) while the issuer is still warming up.
pub async fn cap_jwks(
    Extension(svc): Extension<Arc<Service>>,
) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(svc.cap_jwks().await?))
}

/// `GET /issuers/cap/.well-known/openid-configuration` — capability discovery.
///
/// # Errors
/// Infallible in practice; typed as `ApiResult` for handler-signature parity.
pub async fn cap_discovery(
    Extension(svc): Extension<Arc<Service>>,
) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(svc.cap_discovery()))
}

/// `GET /issuers/grant/jwks.json` — grant-token JWKS.
///
/// # Errors
/// Returns a canonical `Problem` (503) while the issuer is still warming up.
pub async fn grant_jwks(
    Extension(svc): Extension<Arc<Service>>,
) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(svc.grant_jwks().await?))
}

/// `GET /issuers/grant/.well-known/openid-configuration` — grant discovery.
///
/// # Errors
/// Infallible in practice; typed as `ApiResult` for handler-signature parity.
pub async fn grant_discovery(
    Extension(svc): Extension<Arc<Service>>,
) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(svc.grant_discovery()))
}

/// `GET /issuers/obo/jwks.json` — OBO-token JWKS (registered only when
/// `obo.enabled`).
///
/// # Errors
/// Returns a canonical `Problem` (503) while the issuer is still warming up.
pub async fn obo_jwks(
    Extension(svc): Extension<Arc<Service>>,
) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(svc.obo_jwks().await?))
}

/// `GET /issuers/obo/.well-known/openid-configuration` — OBO discovery
/// (registered only when `obo.enabled`).
///
/// # Errors
/// Infallible in practice; typed as `ApiResult` for handler-signature parity.
pub async fn obo_discovery(
    Extension(svc): Extension<Arc<Service>>,
) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(svc.obo_discovery()))
}

/// `POST /internal/v1/issuers/obo/tokens` — re-mint a capability token into a
/// down-scoped OBO token (registered only when `obo.enabled`).
///
/// This route is `.public()`: there is no user/KC bearer here. The auth is the
/// presented **capability token** (`Authorization: Bearer <cap jwt>`, verified
/// in `remint_obo` against the cap JWKS) plus the **mTLS peer identity** (bound
/// to the cap audience). Both are verified in-handler / in-domain.
///
/// The verified client-certificate subject comes from the connection's mTLS
/// layer. That layer is **external** and not yet wired (DESIGN.md § 4.1), so the
/// subject is `None` here and [`crate::domain::peer_identity::RegistryPeerIdentityResolver`]
/// fail-closes with `403`. That is the correct posture while the surface is
/// gated off; the network exposure / mTLS listener lands with that layer.
///
/// SECURITY: when mTLS lands, `client_cert_subject` MUST be sourced from
/// the verified TLS layer's peer certificate — never from a client-supplied
/// forwarded header (e.g. `X-Forwarded-Client-Cert`), which a caller can spoof.
///
/// # Errors
/// Maps the [`DomainError`] from `remint_obo` to RFC 9457: `400` (malformed
/// scope request), `401` (cap provenance / expiry), `403` (peer / adapter /
/// scope / loop-guard), `404` (OBO disabled), or `503` (issuer not ready).
pub async fn remint_obo(
    Extension(svc): Extension<Arc<Service>>,
    headers: axum::http::HeaderMap,
    body: Option<Json<RemintRequest>>,
) -> ApiResult<Json<RemintResponse>> {
    let cap_jwt = bearer_token(&headers)?;
    // mTLS peer cert is supplied by the (external #8) transport layer; absent
    // here → the resolver fail-closes (403). Surface is gated, so this is fine.
    let peer = PeerConnInfo {
        client_cert_subject: None,
    };
    let requested = body.and_then(|Json(b)| b.scopes);
    if let Some(scopes) = requested.as_deref() {
        validate_requested_scopes(scopes)?;
    }
    let token = svc.remint_obo(&peer, &cap_jwt, requested).await?;
    Ok(Json(RemintResponse { token }))
}

/// Max number of `requested` scope entries, and max length per entry.
const MAX_SCOPES: usize = 64;
const MAX_SCOPE_LEN: usize = 256;

/// Bounds the caller-`requested` scope subset: a present-but-empty list is a
/// down-scope to nothing (never minted) and an over-sized / over-long list is a
/// malformed request — both surface as `InvalidRequest` (400).
fn validate_requested_scopes(scopes: &[String]) -> Result<(), DomainError> {
    if scopes.is_empty() {
        return Err(DomainError::InvalidRequest {
            detail: "requested scopes must not be empty".to_owned(),
        });
    }
    if scopes.len() > MAX_SCOPES || scopes.iter().any(|s| s.len() > MAX_SCOPE_LEN) {
        return Err(DomainError::InvalidRequest {
            detail: "requested scopes exceed bounds".to_owned(),
        });
    }
    Ok(())
}

/// Extracts the `Authorization: Bearer <token>` value. A missing or malformed
/// header is a cap-provenance failure (`401`) — there is no token to verify.
fn bearer_token(headers: &axum::http::HeaderMap) -> Result<String, DomainError> {
    let raw = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| DomainError::cap_invalid("missing Authorization header"))?;
    let (scheme, token) = raw
        .split_once(' ')
        .ok_or_else(|| DomainError::cap_invalid("Authorization is not a Bearer token"))?;
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return Err(DomainError::cap_invalid(
            "Authorization is not a Bearer token",
        ));
    }
    let token = token.trim();
    if token.is_empty() {
        return Err(DomainError::cap_invalid(
            "Authorization is not a Bearer token",
        ));
    }
    Ok(token.to_owned())
}

#[cfg(test)]
mod tests {
    use super::bearer_token;

    #[test]
    fn bearer_scheme_is_ascii_case_insensitive() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderValue::from_static("BEARER header.payload.signature"),
        );

        assert!(matches!(
            bearer_token(&headers).as_deref(),
            Ok("header.payload.signature")
        ));
    }

    #[test]
    fn bearer_extractor_rejects_missing_malformed_and_empty_headers() {
        assert!(bearer_token(&axum::http::HeaderMap::new()).is_err());

        for value in ["Bearer", "Basic token", "Bearer   ", "Bearer\ttoken"] {
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(
                axum::http::header::AUTHORIZATION,
                axum::http::HeaderValue::from_static(value),
            );
            assert!(bearer_token(&headers).is_err(), "accepted {value:?}");
        }
    }
}
