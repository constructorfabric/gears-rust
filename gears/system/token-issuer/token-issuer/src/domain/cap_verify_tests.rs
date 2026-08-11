use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use p256::pkcs8::{EncodePublicKey, LineEnding};
use rand_core::OsRng;
use serde_json::json;
use uuid::Uuid;

use super::*;
use crate::domain::jwks::jwks_document;
use token_issuer_sdk::{PublicKeyVersion, SigAlg};

const CAP_ISS: &str = "https://core.example.com/issuers/cap";
const KID: &str = "cap-token-sign-v1";

/// b64url-no-pad JSON encode.
fn b64(v: &serde_json::Value) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(v).unwrap())
}

/// Signs a cap-shaped JWT with a fresh P-256 key and returns the JWT plus the
/// single-key JWKS that verifies it. `typ`/`exp` are parameterized so tests can
/// mutate them.
fn sign_cap_jwt(typ: &str, iss: &str, exp: i64) -> (String, serde_json::Value) {
    let key = SigningKey::random(&mut OsRng);
    let pem = p256::PublicKey::from(key.verifying_key())
        .to_public_key_pem(LineEnding::LF)
        .unwrap();
    let jwks = jwks_document(
        "cap-token-sign",
        &[PublicKeyVersion {
            version: 1,
            alg: SigAlg::Es256,
            public_key_pem: pem,
        }],
    );

    let header = b64(&json!({ "alg": "ES256", "typ": typ, "kid": KID }));
    let claims = json!({
        "iss": iss,
        "aud": "gts.cf.rms._.adapter.v1~acme.rms._.s3.v1",
        "sub": Uuid::from_u128(0x1111),
        "subject_tenant": Uuid::from_u128(0x2222),
        "user_type": "user",
        "context_tenant": Uuid::from_u128(0x42),
        "scopes": "quotas:read",
        "jti": Uuid::from_u128(0x1234),
        "iat": exp - 300,
        "exp": exp,
    });
    let payload = b64(&claims);
    let signing_input = format!("{header}.{payload}");
    let sig: Signature = key.sign(signing_input.as_bytes());
    let jwt = format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(sig.to_bytes()));
    (jwt, jwks)
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

#[test]
fn verifies_cap_token_against_cap_jwks() {
    let (jwt, jwks) = sign_cap_jwt("cap+jwt", CAP_ISS, now() + 300);
    let claims = verify_cap(&jwt, &jwks, CAP_ISS, 30).unwrap();
    assert_eq!(claims.iss, CAP_ISS);
    assert_eq!(claims.scopes, "quotas:read");
    assert_eq!(claims.sub, Uuid::from_u128(0x1111));
}

#[test]
fn rejects_expired_cap_token() {
    // exp 300s in the past, well beyond the 30s leeway.
    let (jwt, jwks) = sign_cap_jwt("cap+jwt", CAP_ISS, now() - 300);
    assert!(matches!(
        verify_cap(&jwt, &jwks, CAP_ISS, 30),
        Err(DomainError::CapInvalid { .. })
    ));
}

#[test]
fn rejects_wrong_typ() {
    let (jwt, jwks) = sign_cap_jwt("obo+jwt", CAP_ISS, now() + 300);
    assert!(matches!(
        verify_cap(&jwt, &jwks, CAP_ISS, 30),
        Err(DomainError::CapInvalid { .. })
    ));
}

#[test]
fn rejects_wrong_issuer() {
    let (jwt, jwks) = sign_cap_jwt("cap+jwt", "https://evil/issuers/cap", now() + 300);
    assert!(matches!(
        verify_cap(&jwt, &jwks, CAP_ISS, 30),
        Err(DomainError::CapInvalid { .. })
    ));
}

#[test]
fn rejects_tampered_signature() {
    let (jwt, jwks) = sign_cap_jwt("cap+jwt", CAP_ISS, now() + 300);
    let mut parts: Vec<String> = jwt.split('.').map(str::to_owned).collect();
    let sig = &parts[2];
    let last = sig.chars().next_back().unwrap();
    let replacement = if last == 'A' { 'B' } else { 'A' };
    let mut chars: Vec<char> = sig.chars().collect();
    let n = chars.len();
    chars[n - 1] = replacement;
    parts[2] = chars.into_iter().collect();
    let tampered = parts.join(".");
    assert!(matches!(
        verify_cap(&tampered, &jwks, CAP_ISS, 30),
        Err(DomainError::CapInvalid { .. })
    ));
}

#[test]
fn rejects_unknown_kid() {
    // Build a JWT signed by one key but present an unrelated JWKS.
    let (jwt, _jwks) = sign_cap_jwt("cap+jwt", CAP_ISS, now() + 300);
    let (_other_jwt, other_jwks) = sign_cap_jwt("cap+jwt", CAP_ISS, now() + 300);
    // Rewrite the JWKS kid so it cannot match the JWT header kid.
    let mut wrong = other_jwks;
    wrong["keys"][0]["kid"] = json!("cap-token-sign-v999");
    assert!(matches!(
        verify_cap(&jwt, &wrong, CAP_ISS, 30),
        Err(DomainError::CapInvalid { .. })
    ));
}
