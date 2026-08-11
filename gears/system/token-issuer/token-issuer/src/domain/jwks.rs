//! JWKS builder: P-256 public-key PEM → EC JWK / JWKS document.
//!
//! Parsing is deliberately **structural DER/PEM decoding only**, via `pkcs8`
//! and its re-exported `spki` — no curve arithmetic and no cryptographic
//! primitives run here. The affine coordinates are read verbatim out of the
//! SPKI `subjectPublicKey` BIT STRING, which for an `id-ecPublicKey` key is
//! already the SEC1 encoding of the point. This keeps a pure-Rust P-256
//! implementation out of the shipped dependency graph and leaves the
//! FIPS-validated boundary with the signing plugin; see ADR 0004 and the
//! `pkcs8`/`sec1` note in the workspace manifest.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use pkcs8::ObjectIdentifier;
use pkcs8::der::Decode;
use pkcs8::spki::SubjectPublicKeyInfoRef;
use token_issuer_sdk::PublicKeyVersion;

/// `id-ecPublicKey` (RFC 5480 §2.1.1).
const ID_EC_PUBLIC_KEY: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
/// `secp256r1` / NIST P-256 (RFC 5480 §2.1.1.1).
const SECP256R1: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.3.1.7");
/// SEC1 tag for an uncompressed point (SEC1 §2.3.3).
const SEC1_UNCOMPRESSED_TAG: u8 = 0x04;
/// Byte length of a single P-256 affine coordinate.
const P256_COORD_LEN: usize = 32;
/// PEM label for a SPKI public key (RFC 7468 §13).
const SPKI_PEM_LABEL: &str = "PUBLIC KEY";

/// Builds an EC JWK (`kty:EC, crv:P-256, alg:ES256, use:sig`) from a P-256
/// public-key PEM, tagged with `kid = {key_name}-v{version}`.
///
/// # Errors
/// Returns `Err` with a description if the PEM label is wrong, the DER is not a
/// parseable SPKI structure, the algorithm is not `id-ecPublicKey` over
/// `secp256r1`, or the encoded point is not a well-formed uncompressed P-256
/// point.
pub fn ec_jwk_from_pem(
    key_name: &str,
    version: u32,
    pem: &str,
) -> Result<serde_json::Value, String> {
    let (label, doc) = pkcs8::Document::from_pem(pem).map_err(|e| e.to_string())?;
    if label != SPKI_PEM_LABEL {
        return Err(format!("unexpected PEM label `{label}`"));
    }

    let spki = SubjectPublicKeyInfoRef::from_der(doc.as_bytes()).map_err(|e| e.to_string())?;
    if spki.algorithm.oid != ID_EC_PUBLIC_KEY {
        return Err(format!("not an EC public key (oid {})", spki.algorithm.oid));
    }
    let curve = spki
        .algorithm
        .parameters_oid()
        .map_err(|e| format!("missing curve parameters: {e}"))?;
    if curve != SECP256R1 {
        return Err(format!("not a P-256 key (curve {curve})"));
    }

    // For `id-ecPublicKey`, subjectPublicKey is the SEC1 point verbatim.
    // Only the uncompressed form carries both coordinates; a compressed point
    // would require curve arithmetic to recover `y`, which is exactly what this
    // module refuses to do.
    let point = spki
        .subject_public_key
        .as_bytes()
        .ok_or("subjectPublicKey is not octet-aligned")?;
    if point.len() != 1 + 2 * P256_COORD_LEN || point[0] != SEC1_UNCOMPRESSED_TAG {
        return Err(format!(
            "expected an uncompressed SEC1 point ({} bytes, tag 0x04), got {} bytes with tag {:#04x}",
            1 + 2 * P256_COORD_LEN,
            point.len(),
            point.first().copied().unwrap_or_default()
        ));
    }
    let x = URL_SAFE_NO_PAD.encode(&point[1..=P256_COORD_LEN]);
    let y = URL_SAFE_NO_PAD.encode(&point[1 + P256_COORD_LEN..]);
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
