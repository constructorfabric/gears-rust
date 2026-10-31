#![cfg(feature = "jwt")]

use aws_lc_rs::signature::KeyPair;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::crypto::{CryptoProvider, JwkUtils, JwtVerifier};
use jsonwebtoken::signature::Verifier;
use jsonwebtoken::{Algorithm, DecodingKey, Header, Validation};
use std::sync::atomic::{AtomicUsize, Ordering};

static CONSTRUCTIONS: AtomicUsize = AtomicUsize::new(0);
struct ForbiddenVerifier(Algorithm);

impl Verifier<Vec<u8>> for ForbiddenVerifier {
    fn verify(&self, _: &[u8], _: &Vec<u8>) -> Result<(), jsonwebtoken::signature::Error> {
        panic!("claim-validation pass must never execute a second crypto backend")
    }
}

impl JwtVerifier for ForbiddenVerifier {
    fn algorithm(&self) -> Algorithm {
        self.0
    }
}

static POISON: CryptoProvider = CryptoProvider {
    signer_factory: |_, _| panic!("no signing in this test"),
    verifier_factory: |algorithm, _| {
        CONSTRUCTIONS.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(ForbiddenVerifier(*algorithm)))
    },
    jwk_utils: JwkUtils::new_unimplemented(),
};

#[test]
fn claims_pass_does_not_execute_jsonwebtoken_crypto() {
    POISON.install_default().unwrap();
    let encoding = jsonwebtoken::EncodingKey::from_rsa_pem(include_bytes!(
        "../src/crypto/testdata/rsa-private.pem"
    ))
    .unwrap();
    let aws_key = aws_lc_rs::signature::RsaKeyPair::from_pkcs8(encoding.inner())
        .or_else(|_| aws_lc_rs::signature::RsaKeyPair::from_der(encoding.inner()))
        .unwrap();
    let key = DecodingKey::from_rsa_der(aws_key.public_key().as_ref());
    let rsa_encoding = encoding;
    let rsa_key = key;
    let mut cases = Vec::new();
    for algorithm in [
        Algorithm::RS256,
        Algorithm::RS384,
        Algorithm::RS512,
        Algorithm::PS256,
        Algorithm::PS384,
        Algorithm::PS512,
    ] {
        cases.push((algorithm, rsa_encoding.clone(), rsa_key.clone()));
    }
    for (algorithm, signing) in [
        (
            Algorithm::ES256,
            &aws_lc_rs::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
        ),
        (
            Algorithm::ES384,
            &aws_lc_rs::signature::ECDSA_P384_SHA384_FIXED_SIGNING,
        ),
    ] {
        let bytes = aws_lc_rs::signature::EcdsaKeyPair::generate_pkcs8(
            signing,
            &aws_lc_rs::rand::SystemRandom::new(),
        )
        .unwrap();
        let key = aws_lc_rs::signature::EcdsaKeyPair::from_pkcs8(signing, bytes.as_ref()).unwrap();
        cases.push((
            algorithm,
            jsonwebtoken::EncodingKey::from_ec_der(bytes.as_ref()),
            DecodingKey::from_ec_der(key.public_key().as_ref()),
        ));
    }
    for (index, (algorithm, encoding, key)) in cases.into_iter().enumerate() {
        let header = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&Header::new(algorithm)).unwrap());
        let payload = URL_SAFE_NO_PAD.encode(br#"{"sub":"alice","exp":4000000000}"#);
        let message = format!("{header}.{payload}");
        // Independent signing oracle; the poisoned global must never verify.
        let signer =
            (jsonwebtoken::crypto::aws_lc::DEFAULT_PROVIDER.signer_factory)(&algorithm, &encoding)
                .unwrap();
        let mut signature = signer.try_sign(message.as_bytes()).unwrap();
        let token = format!("{message}.{}", URL_SAFE_NO_PAD.encode(&signature));
        let decoded = toolkit_security::jwt::decode::<serde_json::Value>(
            &token,
            &key,
            &Validation::new(algorithm),
        )
        .unwrap();
        assert_eq!(decoded.claims["sub"], "alice");
        assert_eq!(CONSTRUCTIONS.load(Ordering::SeqCst), index + 1);
        signature[0] ^= 1;
        let invalid = format!("{message}.{}", URL_SAFE_NO_PAD.encode(signature));
        assert!(
            toolkit_security::jwt::decode::<serde_json::Value>(
                &invalid,
                &key,
                &Validation::new(algorithm)
            )
            .is_err()
        );
        assert_eq!(CONSTRUCTIONS.load(Ordering::SeqCst), index + 1);
    }
}
