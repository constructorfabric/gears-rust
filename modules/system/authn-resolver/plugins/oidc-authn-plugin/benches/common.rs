#![allow(clippy::expect_used, clippy::missing_panics_doc)]

use std::sync::OnceLock;

use jsonwebtoken::jwk::{Jwk, JwkSet, PublicKeyUse};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};

struct TestKeyMaterial {
    encoding_key: EncodingKey,
    jwks_json: String,
}

static KEY_MATERIAL: OnceLock<TestKeyMaterial> = OnceLock::new();

/// Key ID embedded in [`test_jwk_json`].
pub const TEST_KID: &str = "test-key-1";

fn key_material() -> &'static TestKeyMaterial {
    KEY_MATERIAL.get_or_init(build_key_material)
}

fn build_key_material() -> TestKeyMaterial {
    let signing_key = rcgen::KeyPair::generate_for(&rcgen::PKCS_RSA_SHA256)
        .expect("benchmark RSA key should generate");
    let encoding_key = EncodingKey::from_rsa_pem(signing_key.serialize_pem().as_bytes())
        .expect("generated RSA key should encode JWTs");
    let mut jwk = Jwk::from_encoding_key(&encoding_key, Algorithm::RS256)
        .expect("generated RSA key should derive a public JWK");
    jwk.common.key_id = Some(TEST_KID.to_owned());
    jwk.common.public_key_use = Some(PublicKeyUse::Signature);
    let jwks_json =
        serde_json::to_string(&JwkSet { keys: vec![jwk] }).expect("generated JWKS should encode");

    TestKeyMaterial {
        encoding_key,
        jwks_json,
    }
}

#[must_use]
pub fn test_jwk_json() -> &'static str {
    &key_material().jwks_json
}

#[must_use]
pub fn future_exp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(9_999_999_999, |d| d.as_secs() + 3600)
}

#[must_use]
pub fn sign_jwt(claims: &serde_json::Value, kid: Option<&str>) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = kid.map(str::to_owned);
    encode(&header, claims, &key_material().encoding_key).expect("benchmark JWT should sign")
}
