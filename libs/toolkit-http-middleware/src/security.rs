//! Inbound HTTP security extractors.
//!
//! `SecurityContext` propagation over HTTP uses a single header,
//! `Authorization: Bearer <jwt>`, carrying the original tenant-plane JWT. The
//! token is forwarded as-is across hops and **re-validated at every hop** —
//! there is no trusted-peer fast path (zero-trust). The platform plane uses a
//! dedicated `X-ToolKit-Internal-Token` header so system credentials never
//! collide with the tenant-plane user JWT.
//!
//! These extractors are the receiving (server) half; the matching `attach_*`
//! helpers that set the headers on outgoing requests live in `toolkit-http`
//! (the HTTP client).

use http::{
    HeaderMap, HeaderValue,
    header::{AUTHORIZATION, AsHeaderName},
};
use secrecy::SecretString;
use toolkit_security::constants::INTERNAL_TOKEN_HEADER;

/// Errors raised while extracting a bearer token from HTTP request headers.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum SecurityContextHttpError {
    /// No `Authorization` header was present on the request.
    #[error("missing Authorization header")]
    MissingAuthHeader,
    /// The `Authorization` header was present but not a valid `Bearer` token
    /// (non-ASCII bytes, wrong scheme, or no scheme/token separator).
    #[error("invalid Authorization header format")]
    InvalidAuthHeader,
    /// The `Authorization` header used the `Bearer` scheme but the token was
    /// empty after trimming surrounding whitespace.
    #[error("empty bearer token")]
    EmptyToken,
}

/// Extract the raw bearer token from the `Authorization` header.
///
/// The `Bearer` scheme is matched case-insensitively, surrounding whitespace is
/// trimmed, and an empty token is rejected.
///
/// # Errors
///
/// Returns [`SecurityContextHttpError`] when the header is absent, not a valid
/// `Bearer` credential, or carries an empty token.
pub fn extract_bearer_http(headers: &HeaderMap) -> Result<SecretString, SecurityContextHttpError> {
    let header = single_header_value(headers, AUTHORIZATION)
        .map_err(|()| SecurityContextHttpError::InvalidAuthHeader)?
        .ok_or(SecurityContextHttpError::MissingAuthHeader)?;
    let raw = header
        .to_str()
        .map_err(|_| SecurityContextHttpError::InvalidAuthHeader)?;

    let (scheme, token) = raw
        .trim_start()
        .split_once(char::is_whitespace)
        .ok_or(SecurityContextHttpError::InvalidAuthHeader)?;

    if !scheme.eq_ignore_ascii_case("Bearer") {
        return Err(SecurityContextHttpError::InvalidAuthHeader);
    }

    let token = token.trim();
    if token.is_empty() {
        return Err(SecurityContextHttpError::EmptyToken);
    }
    if token.contains(char::is_whitespace) {
        return Err(SecurityContextHttpError::InvalidAuthHeader);
    }

    Ok(SecretString::from(token))
}

/// Errors raised while extracting the platform-plane internal token from HTTP
/// request headers.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum InternalTokenHttpError {
    /// No `X-ToolKit-Internal-Token` header was present on the request.
    #[error("missing X-ToolKit-Internal-Token header")]
    MissingHeader,
    /// The header was present but its value was not valid (non-ASCII bytes).
    #[error("invalid X-ToolKit-Internal-Token header value")]
    InvalidHeader,
    /// The header was present but empty after trimming surrounding whitespace.
    #[error("empty internal token")]
    EmptyToken,
}

/// Extract the raw platform-plane internal token from the
/// `X-ToolKit-Internal-Token` header.
///
/// Surrounding whitespace is trimmed and an empty token is rejected. The token
/// is opaque to the transport — its shape (JWT vs. opaque) is classified, and
/// it is validated, by the receiving `InternalAuthenticator`.
///
/// # Errors
///
/// Returns [`InternalTokenHttpError`] when the header is absent, not valid
/// ASCII, or carries an empty token.
pub fn extract_internal_token_http(
    headers: &HeaderMap,
) -> Result<SecretString, InternalTokenHttpError> {
    let header = single_header_value(headers, INTERNAL_TOKEN_HEADER)
        .map_err(|()| InternalTokenHttpError::InvalidHeader)?
        .ok_or(InternalTokenHttpError::MissingHeader)?;
    let raw = header
        .to_str()
        .map_err(|_| InternalTokenHttpError::InvalidHeader)?;

    let token = raw.trim();
    if token.is_empty() {
        return Err(InternalTokenHttpError::EmptyToken);
    }

    Ok(SecretString::from(token))
}

/// Look up a credential header that may appear at most once.
///
/// Returns `Ok(None)` when the header is absent and `Ok(Some(value))` for
/// exactly one value. Returns `Err(())` when the header appears more than once:
/// duplicate credential headers are rejected as invalid input to avoid
/// request-smuggling ambiguity between a proxy and the application. Callers map
/// the `Err(())` and `None` cases onto their own invalid / missing error
/// variants so both extractors behave consistently.
fn single_header_value<K: AsHeaderName>(
    headers: &HeaderMap,
    name: K,
) -> Result<Option<&HeaderValue>, ()> {
    let mut values = headers.get_all(name).iter();
    let first = values.next();
    if values.next().is_some() {
        return Err(());
    }
    Ok(first)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "security_tests.rs"]
mod tests;
