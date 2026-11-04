use super::{EcdsaP256KeyPair, EcdsaP256VerifyingKey, der_to_fixed, fixed_to_der};
use aws_lc_rs::signature::{ECDSA_P256_SHA256_ASN1, UnparsedPublicKey};
use pem_rfc7468::LineEnding;
use proptest::prelude::*;

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

#[test]
fn fixed_signatures_verify_with_the_aws_lc_fixed_oracle() {
    use aws_lc_rs::signature::ECDSA_P256_SHA256_FIXED;
    let key = EcdsaP256KeyPair::from_pkcs8(KEY).unwrap();
    let verifier = UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, key.public_key());
    // Each signature draws a fresh nonce; short-scalar padding is pinned below.
    for _ in 0..8 {
        let fixed = key.sign_fixed(MESSAGE).unwrap();
        verifier.verify(MESSAGE, &fixed).unwrap();
        assert!(verifier.verify(b"tampered", &fixed).is_err());
    }
}

#[test]
fn der_to_fixed_pads_short_scalars_and_rejects_malformed_sequences() {
    // r = 1 (one byte), s = 2^255 (32 bytes, high bit set so DER prepends 0x00).
    let mut der = vec![0x30, 0x26, 0x02, 0x01, 0x01, 0x02, 0x21, 0x00, 0x80];
    der.extend_from_slice(&[0; 31]);
    let fixed = der_to_fixed(&der, 32).unwrap();
    assert_eq!(fixed[..31], [0; 31]);
    assert_eq!(fixed[31], 1);
    assert_eq!(fixed[32], 0x80);
    assert_eq!(fixed[33..], [0; 31]);
    assert_eq!(fixed_to_der(&fixed, 32).unwrap(), der);
    // The OpenSSL fixture round-trips through the oracle and back to DER.
    let key = EcdsaP256KeyPair::from_pkcs8(KEY).unwrap();
    let fixed = der_to_fixed(SIGNATURE, 32).unwrap();
    UnparsedPublicKey::new(
        &aws_lc_rs::signature::ECDSA_P256_SHA256_FIXED,
        key.public_key(),
    )
    .verify(MESSAGE, &fixed)
    .unwrap();
    assert_eq!(fixed_to_der(&fixed, 32).unwrap(), SIGNATURE);
    // The P-384 width is the other JOSE consumer.
    assert_eq!(
        der_to_fixed(&fixed_to_der(&[7; 96], 48).unwrap(), 48).unwrap(),
        [7; 96]
    );
    let mut oversized = vec![0x30, 0x26, 0x02, 0x21, 0x01];
    oversized.extend_from_slice(&[0; 32]);
    oversized.extend_from_slice(&[0x02, 0x01, 0x01]);
    let mut trailing = SIGNATURE.to_vec();
    trailing.push(0);
    let mut negative = SIGNATURE.to_vec();
    negative[4] |= 0x80;
    for malformed in [
        &[][..],
        &[0; 64][..],
        &SIGNATURE[..SIGNATURE.len() - 1],
        &trailing,
        &oversized,
        &negative,
        &[0x30, 0x03, 0x02, 0x01, 0x01][..],
        // Non-minimal INTEGER (leading zero without a high bit).
        &[0x30, 0x07, 0x02, 0x02, 0x00, 0x01, 0x02, 0x01, 0x01][..],
        // Bytes left inside the SEQUENCE after both scalars.
        &[0x30, 0x08, 0x02, 0x01, 0x01, 0x02, 0x01, 0x01, 0x00, 0x00][..],
    ] {
        assert!(der_to_fixed(malformed, 32).is_err(), "{malformed:02x?}");
    }
    for width in [0, 49, usize::MAX] {
        assert!(der_to_fixed(SIGNATURE, width).is_err(), "{width}");
    }
    for (fixed, width) in [
        (&[1; 63][..], 32),
        (&[1; 65][..], 32),
        (&[1; 64][..], 0),
        (&[1; 98][..], 49),
    ] {
        assert!(fixed_to_der(fixed, width).is_err(), "{width}");
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]
    #[test]
    fn fixed_scalars_round_trip_through_der(
        r in prop::array::uniform32(any::<u8>()),
        s in prop::array::uniform32(any::<u8>()),
    ) {
        prop_assume!(r.iter().any(|b| *b != 0) && s.iter().any(|b| *b != 0));
        let fixed: Vec<u8> = r.iter().chain(&s).copied().collect();
        let der = fixed_to_der(&fixed, 32).unwrap();
        prop_assert_eq!(der_to_fixed(&der, 32).unwrap(), fixed);
    }

    #[test]
    fn der_to_fixed_only_returns_ok_or_err(
        bytes in prop::collection::vec(any::<u8>(), 0..160),
        width in prop::sample::select(vec![32usize, 48]),
    ) {
        if let Ok(fixed) = der_to_fixed(&bytes, width) {
            prop_assert_eq!(fixed.len(), 2 * width);
            prop_assert_eq!(fixed_to_der(&fixed, width).unwrap(), bytes);
        }
    }
}

#[test]
fn debug_output_never_contains_the_private_scalar() {
    use pkcs8::der::Decode as _;
    let pki = pkcs8::PrivateKeyInfoRef::from_der(KEY).unwrap();
    let scalar = sec1::EcPrivateKey::from_der(pki.private_key.as_bytes())
        .unwrap()
        .private_key;
    assert_eq!(scalar.len(), 32);
    let key = EcdsaP256KeyPair::from_pkcs8(KEY).unwrap();
    let rendered = format!("{key:?}");
    assert!(rendered.starts_with("EcdsaP256KeyPair { public_key: ["));
    assert!(rendered.ends_with(".. }"));
    // Byte-list, hex and base64 renderings a backend key type might use.
    let byte_list = format!("{scalar:?}");
    assert!(!rendered.contains(&byte_list[1..byte_list.len() - 1]));
    assert!(!rendered.contains(&hex::encode(scalar)));
    assert!(!rendered.contains(&hex::encode_upper(scalar)));
    let b64 = pem_rfc7468::encode_string("K", LineEnding::LF, scalar).unwrap();
    for line in b64.lines().filter(|line| !line.starts_with('-')) {
        assert!(!rendered.contains(line));
    }
    // A derived Debug would print the whole DER through the backend's key type.
    assert!(!rendered.contains(&hex::encode(KEY)));
    assert!(rendered.len() < 400, "{rendered}");
}

#[test]
fn pem_import_accepts_only_unencrypted_pkcs8() {
    let pem = pem_rfc7468::encode_string("PRIVATE KEY", LineEnding::LF, KEY).unwrap();
    let key = EcdsaP256KeyPair::from_pkcs8_pem(&pem).unwrap();
    assert_eq!(
        key.public_key(),
        EcdsaP256KeyPair::from_pkcs8(KEY).unwrap().public_key()
    );
    assert!(EcdsaP256KeyPair::from_pkcs8_pem(&pem.replace('\n', "\r\n")).is_ok());
    // Helm block scalars and env vars commonly add surrounding whitespace.
    assert!(EcdsaP256KeyPair::from_pkcs8_pem(&format!("\n {pem}\n\n")).is_ok());
    assert!(EcdsaP256KeyPair::from_pkcs8_pem(pem.trim_end()).is_ok());
    for label in ["EC PRIVATE KEY", "ENCRYPTED PRIVATE KEY", "PUBLIC KEY"] {
        let other = pem_rfc7468::encode_string(label, LineEnding::LF, KEY).unwrap();
        assert!(EcdsaP256KeyPair::from_pkcs8_pem(&other).is_err(), "{label}");
    }
    assert!(EcdsaP256KeyPair::from_pkcs8_pem("").is_err());
    assert!(EcdsaP256KeyPair::from_pkcs8_pem(&pem[..64]).is_err());
    // RFC 7468 strict: the same base64 re-wrapped at 76 columns or on one line
    // is rejected, unlike jsonwebtoken's lenient `pem` crate. Pinned so the
    // consumer migration note stays true.
    let body: String = pem.lines().filter(|line| !line.starts_with('-')).collect();
    for width in [76, body.len()] {
        let rewrapped = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            body.as_bytes()
                .chunks(width)
                .map(|chunk| std::str::from_utf8(chunk).unwrap())
                .collect::<Vec<_>>()
                .join("\n")
        );
        assert!(
            EcdsaP256KeyPair::from_pkcs8_pem(&rewrapped).is_err(),
            "{width}"
        );
    }
}
