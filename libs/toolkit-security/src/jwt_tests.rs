use super::{decode, map_signature_error, rsa_public_key, verify_ecdsa};
use crate::crypto::CryptoError;
use aws_lc_rs::signature::{self, KeyPair};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, DecodingKey, Header, Validation, errors::ErrorKind};
use pkcs1::der::{Decode, Encode};
use serde_json::{Value, json};

fn claims() -> Value {
    json!({"sub": "alice", "exp": jsonwebtoken::get_current_timestamp() + 3600,
        "iss": "issuer", "aud": "audience"})
}

fn policy(algorithm: Algorithm) -> Validation {
    let mut validation = Validation::new(algorithm);
    validation.set_issuer(&["issuer"]);
    validation.set_audience(&["audience"]);
    validation
}

#[test]
fn rsa_claim_validation_and_der_formats_remain_compatible() {
    let pem = include_bytes!("crypto/testdata/rsa-private.pem");
    let encoding = jsonwebtoken::EncodingKey::from_rsa_pem(pem).unwrap();
    let aws_key = signature::RsaKeyPair::from_pkcs8(encoding.inner())
        .or_else(|_| signature::RsaKeyPair::from_der(encoding.inner()))
        .unwrap();
    let der = aws_key.public_key().as_ref();
    let pkcs1_key = pkcs1::RsaPublicKey::from_der(der).unwrap();
    let spki = spki::SubjectPublicKeyInfoRef {
        algorithm: spki::AlgorithmIdentifierRef {
            oid: pkcs1::ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.1"),
            parameters: Some(pkcs1::der::asn1::AnyRef::NULL),
        },
        subject_public_key: pkcs1::der::asn1::BitStringRef::from_bytes(der).unwrap(),
    }
    .to_der()
    .unwrap();
    let keys = [
        DecodingKey::from_rsa_der(der),
        DecodingKey::from_rsa_der(&spki),
        DecodingKey::from_rsa_raw_components(
            pkcs1_key.modulus.as_bytes(),
            pkcs1_key.public_exponent.as_bytes(),
        ),
    ];
    for algorithm in [
        Algorithm::RS256,
        Algorithm::RS384,
        Algorithm::RS512,
        Algorithm::PS256,
        Algorithm::PS384,
        Algorithm::PS512,
    ] {
        let header = Header::new(algorithm);
        let expected = claims();
        let token = jsonwebtoken::encode(&header, &expected, &encoding).unwrap();
        for key in &keys {
            assert_eq!(
                decode::<Value>(&token, key, &policy(algorithm))
                    .unwrap()
                    .claims,
                expected
            );
        }
        let mut expired = claims();
        expired["exp"] = json!(1);
        let token = jsonwebtoken::encode(&header, &expired, &encoding).unwrap();
        assert!(matches!(
            decode::<Value>(&token, &keys[0], &policy(algorithm))
                .unwrap_err()
                .kind(),
            ErrorKind::ExpiredSignature
        ));
    }
    let mut constrained = spki;
    // Change rsaEncryption OID's last arc to RSASSA-PSS; do not erase constraints.
    let oid_at = constrained
        .windows(11)
        .position(|w| w == [6, 9, 42, 134, 72, 134, 247, 13, 1, 1, 1])
        .unwrap();
    constrained[oid_at + 10] = 10;
    assert!(rsa_public_key(&constrained).is_err());
    assert!(rsa_public_key(b"malformed").is_err());
}

#[test]
fn ecdsa_fixed_signatures_use_exact_curve_and_hash() {
    for (algorithm, signing, width) in [
        (
            Algorithm::ES256,
            &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            32,
        ),
        (
            Algorithm::ES384,
            &signature::ECDSA_P384_SHA384_FIXED_SIGNING,
            48,
        ),
    ] {
        let rng = aws_lc_rs::rand::SystemRandom::new();
        let private = signature::EcdsaKeyPair::generate_pkcs8(signing, &rng).unwrap();
        let keypair = signature::EcdsaKeyPair::from_pkcs8(signing, private.as_ref()).unwrap();
        let key = DecodingKey::from_ec_der(keypair.public_key().as_ref());
        let encoding = jsonwebtoken::EncodingKey::from_ec_der(private.as_ref());
        let token = jsonwebtoken::encode(&Header::new(algorithm), &claims(), &encoding).unwrap();
        assert!(decode::<Value>(&token, &key, &policy(algorithm)).is_ok());
        let other = if algorithm == Algorithm::ES256 {
            Algorithm::ES384
        } else {
            Algorithm::ES256
        };
        assert!(verify_ecdsa(other, key.as_bytes(), b"message", &vec![1; width * 2]).is_err());
        assert!(verify_ecdsa(algorithm, key.as_bytes(), b"message", &vec![0; width * 2]).is_err());
        assert!(
            verify_ecdsa(
                algorithm,
                key.as_bytes(),
                b"message",
                &vec![1; width * 2 - 1]
            )
            .is_err()
        );
    }
}

#[test]
fn disabled_signature_flag_cannot_bypass_facade_and_claim_policy_is_preserved() {
    let encoding =
        jsonwebtoken::EncodingKey::from_rsa_pem(include_bytes!("crypto/testdata/rsa-private.pem"))
            .unwrap();
    let aws_key = signature::RsaKeyPair::from_pkcs8(encoding.inner())
        .or_else(|_| signature::RsaKeyPair::from_der(encoding.inner()))
        .unwrap();
    let key = DecodingKey::from_rsa_der(aws_key.public_key().as_ref());
    let token = jsonwebtoken::encode(&Header::new(Algorithm::RS256), &claims(), &encoding).unwrap();
    assert!(decode::<Value>(&token, &key, &policy(Algorithm::RS256)).is_ok());
    let mut wrong_issuer = policy(Algorithm::RS256);
    wrong_issuer.set_issuer(&["other"]);
    assert!(matches!(
        decode::<Value>(&token, &key, &wrong_issuer)
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidIssuer
    ));
    let mut wrong_audience = policy(Algorithm::RS256);
    wrong_audience.set_audience(&["other"]);
    assert!(matches!(
        decode::<Value>(&token, &key, &wrong_audience)
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidAudience
    ));
    let mut disabled = policy(Algorithm::RS256);
    #[allow(deprecated)]
    disabled.insecure_disable_signature_validation();
    let parts: Vec<_> = token.split('.').collect();
    let tampered = format!(
        "{}.{}.{}",
        parts[0],
        URL_SAFE_NO_PAD.encode(b"{}"),
        parts[2]
    );
    assert!(decode::<Value>(&tampered, &key, &disabled).is_err());
    assert!(decode::<Value>(&format!("{token}="), &key, &disabled).is_err());
    disabled.algorithms.clear();
    assert!(decode::<Value>(&token, &key, &disabled).is_err());
    let mut mixed = policy(Algorithm::RS256);
    mixed.algorithms.push(Algorithm::HS256);
    assert!(decode::<Value>(&token, &key, &mixed).is_err());
}

#[test]
fn unused_jwt_families_are_rejected_before_claims_pass() {
    for algorithm in [Algorithm::HS256, Algorithm::HS384, Algorithm::HS512] {
        let secret = [0x31; 32];
        let token = jsonwebtoken::encode(
            &Header::new(algorithm),
            &claims(),
            &jsonwebtoken::EncodingKey::from_secret(&secret),
        )
        .unwrap();
        assert!(matches!(
            decode::<Value>(
                &token,
                &DecodingKey::from_secret(&secret),
                &policy(algorithm)
            )
            .unwrap_err()
            .kind(),
            ErrorKind::InvalidAlgorithm
        ));
    }
}

#[test]
fn crypto_backend_failures_remain_operational_errors() {
    for error in [
        CryptoError::FipsUnavailable,
        CryptoError::Sha256Unavailable,
        CryptoError::UnsupportedAlgorithm,
    ] {
        assert!(matches!(
            map_signature_error(error).kind(),
            ErrorKind::Provider(_)
        ));
    }
    for error in [
        CryptoError::InvalidKey,
        CryptoError::InvalidSignature,
        CryptoError::MessageTooLong,
    ] {
        assert!(matches!(
            map_signature_error(error).kind(),
            ErrorKind::InvalidSignature
        ));
    }
}
