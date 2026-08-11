//! Gate 1 provenance: verify a presented capability JWT against the cap JWKS
//! this service serves.
//!
//! Requires ES256, `typ=="cap+jwt"`, a `kid` resolvable in the cap JWKS, the
//! configured cap issuer, and a non-expired `exp` (within `skew`). Audience is
//! NOT validated here — the cap `aud` is the calling adapter's GTS ID, which is
//! bound to the verified mTLS peer by the re-mint orchestration (Gate 1), not
//! by JWT audience matching.

use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use token_issuer_sdk::CapabilityClaims;

use crate::domain::error::DomainError;

/// Verifies a capability JWT's provenance and returns its claims.
///
/// NOTE on expiry: no clock is injected. `jsonwebtoken` 10.x has no injectable
/// clock — it validates `exp`/`nbf` against the real wall clock
/// ([`jsonwebtoken::get_current_timestamp`]) with `leeway = skew`. Tests must
/// therefore set cap-token `exp` relative to the real clock.
///
/// # Errors
/// Returns [`DomainError::CapInvalid`] if the header is malformed, the algorithm
/// is not ES256, `typ` is not `cap+jwt`, the `kid` is missing/unknown, or the
/// signature / issuer / expiry checks fail.
pub fn verify_cap(
    jwt: &str,
    cap_jwks: &serde_json::Value,
    cap_iss: &str,
    skew: u64,
) -> Result<CapabilityClaims, DomainError> {
    let header = decode_header(jwt).map_err(|_| DomainError::cap_invalid("header"))?;
    if header.alg != Algorithm::ES256 {
        return Err(DomainError::cap_invalid("alg"));
    }
    if header.typ.as_deref() != Some("cap+jwt") {
        return Err(DomainError::cap_invalid("typ"));
    }
    let kid = header.kid.ok_or_else(|| DomainError::cap_invalid("kid"))?;
    let key =
        decoding_key_for_kid(cap_jwks, &kid).ok_or_else(|| DomainError::cap_invalid("kid"))?;

    let mut val = Validation::new(Algorithm::ES256);
    val.set_issuer(&[cap_iss]);
    val.leeway = skew;
    // aud is the adapter GTS ID, bound to the peer in Gate 1 — not a JWT-aud check.
    val.validate_aud = false;

    let data = decode::<CapabilityClaims>(jwt, &key, &val)
        .map_err(|e| DomainError::cap_invalid(e.to_string()))?;
    Ok(data.claims)
}

/// Finds the JWK with the given `kid` in a JWKS document and builds an ES256
/// [`DecodingKey`] from its `x`/`y` EC coordinates.
fn decoding_key_for_kid(jwks: &serde_json::Value, kid: &str) -> Option<DecodingKey> {
    let jwk = jwks
        .get("keys")?
        .as_array()?
        .iter()
        .find(|k| k.get("kid").and_then(serde_json::Value::as_str) == Some(kid))?;
    let x = jwk.get("x")?.as_str()?;
    let y = jwk.get("y")?.as_str()?;
    DecodingKey::from_ec_components(x, y).ok()
}

#[cfg(test)]
#[path = "cap_verify_tests.rs"]
mod tests;
