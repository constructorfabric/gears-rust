//! Unit-test key material and helpers for the oidc-authn-plugin crate.
//!
//! This module is gated behind `#[cfg(test)]` so nothing is compiled into
//! production builds.

use std::sync::OnceLock;

use jsonwebtoken::jwk::{Jwk, JwkSet, PublicKeyUse};
use jsonwebtoken::{Algorithm, EncodingKey};

struct TestKeyMaterial {
    encoding_key: EncodingKey,
    jwks_json: String,
}

static KEY_MATERIAL: OnceLock<TestKeyMaterial> = OnceLock::new();

fn key_material() -> &'static TestKeyMaterial {
    KEY_MATERIAL.get_or_init(build_key_material)
}

fn build_key_material() -> TestKeyMaterial {
    let signing_key = rcgen::KeyPair::generate_for(&rcgen::PKCS_RSA_SHA256)
        .unwrap_or_else(|error| panic!("test RSA key should generate: {error}"));
    let encoding_key = EncodingKey::from_rsa_pem(signing_key.serialize_pem().as_bytes())
        .unwrap_or_else(|error| panic!("generated RSA key should encode JWTs: {error}"));
    let mut jwk = Jwk::from_encoding_key(&encoding_key, Algorithm::RS256)
        .unwrap_or_else(|error| panic!("generated RSA key should derive a public JWK: {error}"));
    jwk.common.key_id = Some(TEST_KID.to_owned());
    jwk.common.public_key_use = Some(PublicKeyUse::Signature);
    let jwks_json = serde_json::to_string(&JwkSet { keys: vec![jwk] })
        .unwrap_or_else(|error| panic!("generated JWKS should encode: {error}"));

    TestKeyMaterial {
        encoding_key,
        jwks_json,
    }
}

/// JWK Set JSON containing the public key matching the generated test signer.
#[must_use]
pub fn test_jwk_json() -> &'static str {
    &key_material().jwks_json
}

/// Default test issuer matching the OIDC realm used in tests.
pub const TEST_ISSUER: &str = "https://oidc.example.com/realms/platform";

/// Key ID embedded in [`test_jwk_json`].
pub const TEST_KID: &str = "test-key-1";

/// Return a Unix timestamp 1 hour in the future (for `exp` claims).
#[must_use]
pub fn future_exp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(9_999_999_999, |d| d.as_secs() + 3600)
}

/// Return a Unix timestamp 1 hour in the past (for expired token tests).
#[must_use]
pub fn past_exp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs().saturating_sub(3600))
}

/// Sign a JWT with the test RS256 private key.
#[must_use]
pub fn sign_jwt(claims: &serde_json::Value, kid: Option<&str>) -> String {
    use jsonwebtoken::{Header, encode};
    let mut header = Header::new(Algorithm::RS256);
    header.kid = kid.map(str::to_owned);
    encode(&header, claims, &key_material().encoding_key).unwrap_or_default()
}

/// Build a [`Claims`] map from a JSON object.
///
/// Common helper shared across unit and integration tests to avoid duplicating
/// the same claims-map construction boilerplate.
///
/// # Panics
///
/// Panics when `value` is not a JSON object.
#[must_use]
pub fn claims(value: serde_json::Value) -> crate::domain::claim_mapper::Claims {
    let serde_json::Value::Object(claims) = value else {
        panic!("claims helper expects a JSON object");
    };
    claims
}
