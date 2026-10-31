use super::{CryptoError, RsaAlgorithm, validate_components, verify_rsa_components};
#[cfg(feature = "jwt")]
use super::{SignatureScheme, verify_with_provider};

#[test]
fn rejects_noncanonical_or_weak_public_components() {
    let valid = vec![0xff; 256];
    assert_eq!(validate_components(&valid, &[1, 0, 1]), Ok(()));
    for n in [vec![], vec![0; 256], vec![0xff; 255], vec![0xff; 1025]] {
        assert_eq!(validate_components(&n, &[3]), Err(CryptoError::InvalidKey));
    }
    let mut too_small = valid.clone();
    too_small[0] = 0x7f;
    assert_eq!(
        validate_components(&too_small, &[3]),
        Err(CryptoError::InvalidKey)
    );
    let mut even = valid.clone();
    even[255] = 0xfe;
    assert_eq!(
        validate_components(&even, &[3]),
        Err(CryptoError::InvalidKey)
    );
    for e in [
        vec![],
        vec![0, 3],
        vec![1],
        vec![2],
        vec![4],
        vec![2, 0, 0, 0, 1],
    ] {
        assert_eq!(
            validate_components(&valid, &e),
            Err(CryptoError::InvalidKey)
        );
    }
    assert_eq!(
        validate_components(&vec![0xff; 1024], &[1, 0xff, 0xff, 0xff, 0xff]),
        Ok(())
    );
}

#[test]
fn rejects_wrong_signature_size_before_dispatch() {
    assert_eq!(
        verify_rsa_components(
            RsaAlgorithm::Pkcs1Sha256,
            &vec![0xff; 256],
            &[3],
            b"message",
            &[0; 255]
        ),
        Err(CryptoError::InvalidSignature)
    );
}

#[cfg(feature = "jwt")]
#[test]
fn provider_mapping_cannot_substitute_a_different_rsa_hash() {
    use aws_lc_rs::{
        rand::SystemRandom,
        signature::{self, KeyPair},
    };
    use pkcs1::der::Decode;

    let encoding =
        jsonwebtoken::EncodingKey::from_rsa_pem(include_bytes!("testdata/rsa-private.pem"))
            .unwrap();
    let key = signature::RsaKeyPair::from_pkcs8(encoding.inner())
        .or_else(|_| signature::RsaKeyPair::from_der(encoding.inner()))
        .unwrap();
    let public = pkcs1::RsaPublicKey::from_der(key.public_key().as_ref()).unwrap();
    let message = b"exact RSA hash selection";
    let mut signature = vec![0; key.public_modulus_len()];
    key.sign(
        &signature::RSA_PKCS1_SHA384,
        &SystemRandom::new(),
        message,
        &mut signature,
    )
    .unwrap();
    let mut provider = rustls::crypto::aws_lc_rs::default_provider();
    let sha384 = provider
        .signature_verification_algorithms
        .mapping
        .iter()
        .find(|(scheme, _)| *scheme == SignatureScheme::RSA_PKCS1_SHA384)
        .unwrap()
        .1;
    provider.signature_verification_algorithms.mapping =
        Box::leak(vec![(SignatureScheme::RSA_PKCS1_SHA256, sha384)].into_boxed_slice());
    assert_eq!(
        verify_with_provider(
            &provider,
            RsaAlgorithm::Pkcs1Sha256,
            public.modulus.as_bytes(),
            public.public_exponent.as_bytes(),
            message,
            &signature
        ),
        Err(CryptoError::InvalidSignature)
    );
}
