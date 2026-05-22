//! Token type detection — distinguishes JWT tokens from opaque tokens.
//!
//! A JWT is identified by exactly three dot-separated segments.
//! The first two segments (header + payload) must be non-empty.
//! The signature segment may be empty for unsecured tokens (`alg: none`).
//! All other bearer tokens are treated as opaque and rejected by auth policy.

use modkit_macros::domain_model;

/// The detected type of a bearer token.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenType {
    /// A JSON Web Token (header.payload.signature format).
    Jwt,
    /// An opaque token (anything that is not a three-segment JWT).
    Opaque,
}

/// Detect whether a bearer token is a JWT or an opaque token.
///
/// A token is classified as [`TokenType::Jwt`] if and only if:
/// 1. It consists of exactly three dot-separated segments.
/// 2. The first two segments (header and payload) are non-empty.
/// 3. The header segment starts with `eyJ` -- the base64url encoding of `{"`,
///    which every valid JOSE header begins with.
///
/// The third segment may be empty (`header.payload.`), which is valid for
/// unsecured JWT encodings and must still be handled as JWT.
///
/// The `eyJ` prefix check filters obviously non-JWT tokens that happen to
/// contain two dots (e.g. `abc.def.ghi`), avoiding unnecessary work on
/// the JWT validation path.
///
/// # Examples
///
/// ```
/// use oidc_authn_plugin::domain::token_type::{TokenType, detect_token_type};
/// use base64::Engine as _;
///
/// // Build a JWT-shaped token at runtime (avoids hardcoded eyJ... literals).
/// let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
///     .encode(r#"{"alg":"RS256"}"#);
/// let jwt = format!("{header}.payload.sig");
///
/// assert_eq!(detect_token_type(&jwt), TokenType::Jwt);
/// assert_eq!(detect_token_type("opaque-token"), TokenType::Opaque);
/// assert_eq!(detect_token_type("abc.def.ghi"), TokenType::Opaque);
/// ```
#[must_use]
pub fn detect_token_type(token: &str) -> TokenType {
    let parts = token.splitn(4, '.');
    let mut count = 0;

    for part in parts {
        if count == 0 && !part.starts_with("eyJ") || count == 1 && part.is_empty() || count == 3 {
            return TokenType::Opaque;
        }

        count += 1;
    }

    if count == 3 {
        TokenType::Jwt
    } else {
        TokenType::Opaque
    }
}

#[cfg(test)]
#[path = "token_type_tests.rs"]
mod token_type_tests;
