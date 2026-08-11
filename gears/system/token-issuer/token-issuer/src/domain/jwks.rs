//! JWKS builder: P-256 public-key PEM → EC JWK / JWKS document.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::pkcs8::DecodePublicKey;
use token_issuer_sdk::PublicKeyVersion;

/// Builds an EC JWK (`kty:EC, crv:P-256, alg:ES256, use:sig`) from a P-256
/// public-key PEM, tagged with `kid = {key_name}-v{version}`.
///
/// # Errors
/// Returns `Err` with a description if the PEM is not a parseable P-256 public
/// key or lacks affine coordinates.
pub fn ec_jwk_from_pem(
    key_name: &str,
    version: u32,
    pem: &str,
) -> Result<serde_json::Value, String> {
    let pk = p256::PublicKey::from_public_key_pem(pem).map_err(|e| e.to_string())?;
    let pt = pk.to_encoded_point(false);
    let x = URL_SAFE_NO_PAD.encode(pt.x().ok_or("missing x coordinate")?);
    let y = URL_SAFE_NO_PAD.encode(pt.y().ok_or("missing y coordinate")?);
    Ok(serde_json::json!({
        "kty": "EC",
        "crv": "P-256",
        "alg": "ES256",
        "use": "sig",
        "kid": format!("{key_name}-v{version}"),
        "x": x,
        "y": y,
    }))
}

/// Builds a JWKS document (`{ "keys": [...] }`) for all published versions of a
/// signing key. Versions whose PEM fails to parse are skipped.
#[must_use]
pub fn jwks_document(key_name: &str, versions: &[PublicKeyVersion]) -> serde_json::Value {
    let keys: Vec<serde_json::Value> = versions
        .iter()
        .filter_map(|v| ec_jwk_from_pem(key_name, v.version, &v.public_key_pem).ok())
        .collect();
    serde_json::json!({ "keys": keys })
}

#[cfg(test)]
#[path = "jwks_tests.rs"]
mod tests;
