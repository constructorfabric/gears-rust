use token_issuer_sdk::SigAlg;

use super::*;

// A fixed, valid P-256 public key (generated with
// `openssl ecparam -genkey -name prime256v1 -noout | openssl ec -pubout`).
const TEST_P256_PUB_PEM: &str = "-----BEGIN PUBLIC KEY-----\n\
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE4npvEkAuDTkb6GJdGJnU/oCgr5VJ\n\
xIzdyl82OX+XlMahydMfbhzeiXqUMAR9Mepi+H9Oym8FxaIvzgheZDp9Kw==\n\
-----END PUBLIC KEY-----\n";

#[test]
fn builds_ec_jwk_from_pem() {
    let jwk = ec_jwk_from_pem("cap-token-sign", 1, TEST_P256_PUB_PEM).unwrap();
    assert_eq!(jwk["kty"], "EC");
    assert_eq!(jwk["crv"], "P-256");
    assert_eq!(jwk["alg"], "ES256");
    assert_eq!(jwk["use"], "sig");
    assert_eq!(jwk["kid"], "cap-token-sign-v1");
    // P-256 coordinates are 32 bytes → 43 base64url chars (no padding).
    assert_eq!(jwk["x"].as_str().unwrap().len(), 43);
    assert_eq!(jwk["y"].as_str().unwrap().len(), 43);
}

#[test]
fn rejects_invalid_pem() {
    assert!(ec_jwk_from_pem("cap-token-sign", 1, "not a pem").is_err());
}

#[test]
fn jwks_document_collects_versions() {
    let versions = vec![PublicKeyVersion {
        version: 1,
        alg: SigAlg::Es256,
        public_key_pem: TEST_P256_PUB_PEM.to_owned(),
    }];
    let doc = jwks_document("cap-token-sign", &versions);
    let keys = doc["keys"].as_array().unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0]["kid"], "cap-token-sign-v1");
}
