use super::{EcdsaP256KeyPair, EcdsaP256VerifyingKey};
use aws_lc_rs::signature::{ECDSA_P256_SHA256_ASN1, UnparsedPublicKey};

const KEY: &[u8] = include_bytes!("../../tests/fixtures/p256-test-key.pk8");
const MESSAGE: &[u8] = include_bytes!("../../tests/fixtures/p256-message.txt");
const SIGNATURE: &[u8] = include_bytes!("../../tests/fixtures/p256-signature.der");

#[test]
fn openssl_signature_and_native_signatures_interoperate() {
    let key = EcdsaP256KeyPair::from_pkcs8(KEY).unwrap();
    let verifier = key.verifying_key();
    verifier.verify(MESSAGE, SIGNATURE).unwrap();
    assert!(verifier.verify(b"tampered", SIGNATURE).is_err());
    let signature = key.sign(MESSAGE).unwrap();
    // Independent oracle: native macOS/Windows signatures must verify in AWS-LC.
    UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, key.public_key())
        .verify(MESSAGE, &signature)
        .unwrap();
}

#[test]
fn generated_keys_round_trip_through_pkcs8() {
    for _ in 0..3 {
        let bytes = EcdsaP256KeyPair::generate_pkcs8().unwrap();
        let key = EcdsaP256KeyPair::from_pkcs8(&bytes).unwrap();
        let signature = key.sign(MESSAGE).unwrap();
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, key.public_key())
            .verify(MESSAGE, &signature)
            .unwrap();
    }
}

#[test]
fn rejects_seeds_wrong_curves_and_inconsistent_public_points() {
    for length in [0, 31, 32, 33, 65] {
        assert!(EcdsaP256KeyPair::from_pkcs8(&vec![0; length]).is_err());
    }
    let mut corrupted = KEY.to_vec();
    // The embedded public point must be checked, not trusted from the envelope.
    *corrupted.last_mut().unwrap() ^= 1;
    assert!(EcdsaP256KeyPair::from_pkcs8(&corrupted).is_err());
    let other = EcdsaP256KeyPair::generate().unwrap();
    let mut mismatched = KEY.to_vec();
    let point_at = mismatched.len() - 65;
    mismatched[point_at..].copy_from_slice(other.public_key());
    assert!(EcdsaP256KeyPair::from_pkcs8(&mismatched).is_err());
    let mut wrong_curve = KEY.to_vec();
    let curve = wrong_curve
        .windows(10)
        .position(|w| w == [6, 8, 42, 134, 72, 206, 61, 3, 1, 7])
        .unwrap();
    wrong_curve[curve + 9] = 8;
    assert!(EcdsaP256KeyPair::from_pkcs8(&wrong_curve).is_err());
    assert!(EcdsaP256VerifyingKey::from_sec1_bytes(&[0; 65]).is_err());
    let key = EcdsaP256KeyPair::from_pkcs8(KEY).unwrap();
    for signature in [&[][..], &[0; 64][..], &[0; 72][..]] {
        assert!(key.verifying_key().verify(MESSAGE, signature).is_err());
    }
}
