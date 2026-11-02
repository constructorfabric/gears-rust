//! Execute CNG primitives even on CI hosts without Windows FIPS policy.
//! Internal injection does not change the production provider/witness gate.
use super::{CryptoError, backend, ecdsa, hash, initialize, mac, rng, rsa};
use aws_lc_rs::signature::{self, KeyPair};
use pkcs1::der::Decode;

#[test]
fn native_cng_operations() {
    if !backend::configured_provider().fips() {
        assert_eq!(initialize(), Err(CryptoError::FipsUnavailable));
        // In particular, long-key normalization must return an error, not panic.
        assert!(matches!(
            mac::HmacSha256::new(&[7; 65]),
            Err(CryptoError::FipsUnavailable)
        ));
        assert_eq!(
            ecdsa::EcdsaP256KeyPair::generate_pkcs8().err(),
            Some(CryptoError::FipsUnavailable)
        );
    }
    let provider = rustls_cng_crypto::custom_provider(
        rustls_cng_crypto::ALL_CIPHER_SUITES.to_vec(),
        rustls_cng_crypto::ALL_KX_GROUPS.to_vec(),
    );
    let suite = backend::find_sha256(&provider).unwrap();
    let mut sha = hash::Sha256::with_suite(suite);
    sha.update(b"abc");
    assert_eq!(
        hex::encode(sha.finalize()),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let mac = mac::HmacSha256::with_suite(&[0x0b; 20], suite).unwrap();
    assert_eq!(
        hex::encode(mac.sign(b"Hi There").unwrap()),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );
    let mac = mac::HmacSha256::with_suite(&[b'B'; 65], suite).unwrap();
    assert_eq!(
        hex::encode(mac.sign(b"boundary").unwrap()),
        "76c506d36076cba6b3ee21d9956fc6fbf3c9d71fe33e2868063af84c08dde198"
    );
    let mut a = [0; 64];
    let mut b = [0; 64];
    rng::fill_with(provider.secure_random, &mut a).unwrap();
    rng::fill_with(provider.secure_random, &mut b).unwrap();
    assert_ne!(a, [0; 64]);
    assert_ne!(a, b);
    let encoding =
        jsonwebtoken::EncodingKey::from_rsa_pem(include_bytes!("testdata/rsa-private.pem"))
            .unwrap();
    let key = signature::RsaKeyPair::from_pkcs8(encoding.inner())
        .or_else(|_| signature::RsaKeyPair::from_der(encoding.inner()))
        .unwrap();
    let public = pkcs1::RsaPublicKey::from_der(key.public_key().as_ref()).unwrap();
    for (algorithm, signing) in [
        (
            rsa::RsaAlgorithm::Pkcs1Sha256,
            &signature::RSA_PKCS1_SHA256 as &dyn signature::RsaEncoding,
        ),
        (rsa::RsaAlgorithm::Pkcs1Sha384, &signature::RSA_PKCS1_SHA384),
        (rsa::RsaAlgorithm::Pkcs1Sha512, &signature::RSA_PKCS1_SHA512),
        (rsa::RsaAlgorithm::PssSha256, &signature::RSA_PSS_SHA256),
        (rsa::RsaAlgorithm::PssSha384, &signature::RSA_PSS_SHA384),
        (rsa::RsaAlgorithm::PssSha512, &signature::RSA_PSS_SHA512),
    ] {
        let mut signature = vec![0; key.public_modulus_len()];
        key.sign(
            signing,
            &aws_lc_rs::rand::SystemRandom::new(),
            b"CNG RSA",
            &mut signature,
        )
        .unwrap();
        rsa::verify_with_provider(
            &provider,
            algorithm,
            public.modulus.as_bytes(),
            public.public_exponent.as_bytes(),
            b"CNG RSA",
            &signature,
        )
        .unwrap();
        signature[0] ^= 1;
        assert!(
            rsa::verify_with_provider(
                &provider,
                algorithm,
                public.modulus.as_bytes(),
                public.public_exponent.as_bytes(),
                b"CNG RSA",
                &signature
            )
            .is_err()
        );
    }
    for (algorithm, signing) in [
        (
            ecdsa::EcdsaAlgorithm::P256Sha256,
            &signature::ECDSA_P256_SHA256_ASN1_SIGNING,
        ),
        (
            ecdsa::EcdsaAlgorithm::P384Sha384,
            &signature::ECDSA_P384_SHA384_ASN1_SIGNING,
        ),
    ] {
        let private =
            signature::EcdsaKeyPair::generate_pkcs8(signing, &aws_lc_rs::rand::SystemRandom::new())
                .unwrap();
        let key = signature::EcdsaKeyPair::from_pkcs8(signing, private.as_ref()).unwrap();
        let signature = key
            .sign(&aws_lc_rs::rand::SystemRandom::new(), b"CNG ECDSA")
            .unwrap();
        ecdsa::verify_signature_with_provider(
            &provider,
            algorithm,
            key.public_key().as_ref(),
            b"CNG ECDSA",
            signature.as_ref(),
        )
        .unwrap();
        assert!(
            ecdsa::verify_signature_with_provider(
                &provider,
                algorithm,
                key.public_key().as_ref(),
                b"changed",
                signature.as_ref()
            )
            .is_err()
        );
    }
    // CNG key generation, import, pairwise check and signing, independent of policy.
    let private = ecdsa::generate_pkcs8().unwrap();
    let key = ecdsa::EcdsaP256KeyPair::from_pkcs8_with_provider(&private, &provider).unwrap();
    let signature = key.sign(b"CNG signing").unwrap();
    signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_ASN1, key.public_key())
        .verify(b"CNG signing", &signature)
        .unwrap();
    let aws_import =
        signature::EcdsaKeyPair::from_pkcs8(&signature::ECDSA_P256_SHA256_ASN1_SIGNING, &private)
            .unwrap();
    assert_eq!(aws_import.public_key().as_ref(), key.public_key());
    let fixture_key = ecdsa::EcdsaP256KeyPair::from_pkcs8_with_provider(
        include_bytes!("../../tests/fixtures/p256-test-key.pk8"),
        &provider,
    )
    .unwrap();
    ecdsa::verify_with_provider(
        &provider,
        fixture_key.public_key(),
        include_bytes!("../../tests/fixtures/p256-message.txt"),
        include_bytes!("../../tests/fixtures/p256-signature.der"),
    )
    .unwrap();
}
