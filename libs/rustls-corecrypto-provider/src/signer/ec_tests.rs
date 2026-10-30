use super::*;
use rcgen::{KeyPair, PKCS_ECDSA_P256_SHA256, PKCS_ECDSA_P384_SHA384, PKCS_ECDSA_P521_SHA512};
use rustls::pki_types::pem::PemObject;

fn gen_ec_key(alg: &'static rcgen::SignatureAlgorithm) -> PrivateKeyDer<'static> {
    let kp = KeyPair::generate_for(alg).expect("rcgen EC");
    let pem = kp.serialize_pem();
    PrivateKeyDer::from_pem_slice(pem.as_bytes()).expect("decode PEM")
}

/// Re-encode a PKCS#8-wrapped EC key as a stand-alone SEC1
/// `ECPrivateKey` DER with the curve OID materialised in the SEC1
/// `parameters` field. The inner OCTET STRING of the PKCS#8 wrapper
/// omits `parameters` (the curve lives in the outer envelope), so
/// we re-encode through `sec1::EcPrivateKey` to produce what
/// `openssl ec -in pkcs8.pem` would produce — the format `EcSigningKey`
/// must accept on the `PrivateKeyDer::Sec1` arm.
fn pkcs8_to_sec1_ec(pkcs8: &PrivateKeyDer<'_>) -> PrivateKeyDer<'static> {
    use pkcs8::der::Encode as _;
    use rustls::pki_types::PrivateSec1KeyDer;
    use sec1::EcParameters;

    let pkcs8_bytes = match pkcs8 {
        PrivateKeyDer::Pkcs8(p) => p.secret_pkcs8_der(),
        other => panic!("expected PKCS#8 input, got {other:?}"),
    };
    let info = pkcs8::PrivateKeyInfoRef::try_from(pkcs8_bytes).expect("parse PKCS#8");
    assert_eq!(info.algorithm.oid, ID_EC_PUBLIC_KEY, "test helper EC only");
    let curve_oid: ObjectIdentifier = info
        .algorithm
        .parameters
        .expect("PKCS#8 EC parameters")
        .decode_as()
        .expect("decode curve OID");

    // Parse the inner SEC1 OCTET STRING, then re-set its parameters
    // field so the resulting DER is a standalone bare-SEC1 key.
    let inner =
        sec1::EcPrivateKey::try_from(info.private_key.as_bytes()).expect("parse inner SEC1");
    let with_params = sec1::EcPrivateKey {
        private_key: inner.private_key,
        parameters: Some(EcParameters::NamedCurve(curve_oid)),
        public_key: inner.public_key,
    };
    let der = with_params.to_der().expect("encode SEC1");
    PrivateKeyDer::Sec1(PrivateSec1KeyDer::from(der))
}

/// Loading + immediate usability per curve. Subsumes the trivial
/// `loads_pkcs8_p*` / `algorithm_reports_ecdsa` tests: builds the
/// signer, asserts the curve was auto-detected correctly, asserts
/// `algorithm()` returns ECDSA, and asserts the signer can actually
/// produce a non-empty signature.
fn pkcs8_load_and_usable(alg: &'static rcgen::SignatureAlgorithm, expected: EcCurve) {
    let der = gen_ec_key(alg);
    let key = EcSigningKey::new(&der).expect("load EC");
    assert_eq!(key.curve, expected);
    assert_eq!(key.algorithm(), SignatureAlgorithm::ECDSA);
    let signer = key.choose_scheme(&[expected.scheme()]).expect("scheme");
    let sig = signer.sign(b"smoke").expect("sign");
    assert!(!sig.is_empty(), "ECDSA signature must not be empty");
}

#[test]
fn pkcs8_p256_load_and_usable() {
    pkcs8_load_and_usable(&PKCS_ECDSA_P256_SHA256, EcCurve::P256);
}

#[test]
fn pkcs8_p384_load_and_usable() {
    pkcs8_load_and_usable(&PKCS_ECDSA_P384_SHA384, EcCurve::P384);
}

#[test]
fn pkcs8_p521_load_and_usable() {
    pkcs8_load_and_usable(&PKCS_ECDSA_P521_SHA512, EcCurve::P521);
}

/// SEC1 path: real handshake_smoke covers PKCS#8 server keys, but
/// some operators ship bare SEC1 PEM. Strip the wrapper and feed it
/// in; verify the curve is detected and a signature roundtrips.
fn sec1_roundtrip(alg: &'static rcgen::SignatureAlgorithm, expected: EcCurve) {
    let pkcs8 = gen_ec_key(alg);
    let sec1 = pkcs8_to_sec1_ec(&pkcs8);
    let key = EcSigningKey::new(&sec1).expect("load SEC1");
    assert_eq!(key.curve, expected);
    let signer = key.choose_scheme(&[expected.scheme()]).expect("scheme");
    let msg = b"sec1 roundtrip";
    let sig = signer.sign(msg).expect("sign");
    let pub_bytes = key
        .key
        .public_key()
        .expect("pub")
        .external_representation()
        .expect("ext")
        .bytes()
        .to_vec();
    crate::verify::SUPPORTED_SIG_ALGS
        .mapping
        .iter()
        .find(|(s, _)| *s == expected.scheme())
        .and_then(|(_, a)| a.first())
        .expect("scheme in mapping")
        .verify_signature(&pub_bytes, msg, &sig)
        .expect("verify SEC1-loaded signature");
}

#[test]
fn sec1_p256_roundtrip() {
    sec1_roundtrip(&PKCS_ECDSA_P256_SHA256, EcCurve::P256);
}

#[test]
fn sec1_p384_roundtrip() {
    sec1_roundtrip(&PKCS_ECDSA_P384_SHA384, EcCurve::P384);
}

#[test]
fn sec1_p521_roundtrip() {
    sec1_roundtrip(&PKCS_ECDSA_P521_SHA512, EcCurve::P521);
}

/// SEC1 bytes that don't decode as a valid `EcPrivateKey` must
/// surface a clean parse error rather than panicking or being
/// silently re-routed through some other arm.
#[test]
fn sec1_garbage_rejected_cleanly() {
    use rustls::pki_types::PrivateSec1KeyDer;
    let bogus = PrivateKeyDer::Sec1(PrivateSec1KeyDer::from(vec![0u8; 32]));
    match EcSigningKey::new(&bogus) {
        Err(Error::General(msg)) => assert!(
            msg.contains("SEC1 parse failed"),
            "error must explain SEC1 parse failure, got {msg:?}"
        ),
        other => panic!("expected Error::General for garbage SEC1, got {other:?}"),
    }
}

/// Wrong-encoding rejection: PKCS#1 is for RSA keys, must be refused
/// by the EC constructor with a documented marker string.
#[test]
fn rejects_pkcs1_input_as_not_ec() {
    use rustls::pki_types::PrivatePkcs1KeyDer;
    let bogus = PrivateKeyDer::Pkcs1(PrivatePkcs1KeyDer::from(vec![0u8; 32]));
    match EcSigningKey::new(&bogus) {
        Err(Error::General(msg)) => assert!(
            msg.contains("PKCS#1 is an RSA encoding"),
            "error must explain PKCS#1 mismatch, got {msg:?}"
        ),
        other => panic!("expected Error::General for PKCS#1, got {other:?}"),
    }
}

/// **FIPS-boundary gate.** A SEC1 key with the `publicKey` OPTIONAL
/// field omitted must be rejected with the documented marker error.
/// The corecrypto provider explicitly does NOT derive `Q = d·G` from
/// the private scalar — that would be an EC scalar-multiplication on
/// secret material outside Apple's FIPS-validated module. If a real
/// regression reintroduced public-point derivation (e.g. by pulling
/// `p256` back in and calling `sk.public_key()`), this test would
/// silently accept the bare-scalar SEC1 instead of failing — making
/// the regression visible.
#[test]
fn sec1_without_public_key_is_rejected() {
    use pkcs8::der::Encode as _;
    use rustls::pki_types::PrivateSec1KeyDer;
    use sec1::EcParameters;

    // Start from a normal PKCS#8 P-256 key, strip the publicKey, and
    // re-encode as bare SEC1. This is the format `openssl ec
    // -no_public` would produce.
    let pkcs8 = gen_ec_key(&PKCS_ECDSA_P256_SHA256);
    let pkcs8_bytes = match &pkcs8 {
        PrivateKeyDer::Pkcs8(p) => p.secret_pkcs8_der(),
        _ => unreachable!(),
    };
    let info = pkcs8::PrivateKeyInfoRef::try_from(pkcs8_bytes).expect("PKCS#8");
    let inner = sec1::EcPrivateKey::try_from(info.private_key.as_bytes()).expect("inner SEC1");
    let stripped = sec1::EcPrivateKey {
        private_key: inner.private_key,
        parameters: Some(EcParameters::NamedCurve(SECP256R1_OID)),
        public_key: None,
    };
    let der = stripped.to_der().expect("re-encode");
    let bare = PrivateKeyDer::Sec1(PrivateSec1KeyDer::from(der));

    match EcSigningKey::new(&bare) {
        Err(Error::General(msg)) => {
            assert!(
                msg.contains("missing the embedded publicKey field"),
                "error must explain missing-publicKey, got {msg:?}"
            );
            assert!(
                msg.contains("outside the FIPS-validated"),
                "error must explain the FIPS-boundary rationale, got {msg:?}"
            );
        }
        other => panic!("expected Error::General for SEC1 missing publicKey, got {other:?}"),
    }
}

/// **FIPS-boundary gate, compressed-point variant.** SEC1 keys whose
/// `publicKey` is in compressed form (`0x02 || X` or `0x03 || X`)
/// must be rejected. Decompressing to `0x04 || X || Y` would require
/// a square-root computation over the curve's prime field — a
/// cryptographic primitive outside Apple corecrypto. Our policy is
/// to refuse rather than re-derive.
#[test]
fn sec1_with_compressed_public_key_is_rejected() {
    use pkcs8::der::Encode as _;
    use rustls::pki_types::PrivateSec1KeyDer;
    use sec1::EcParameters;

    let pkcs8 = gen_ec_key(&PKCS_ECDSA_P256_SHA256);
    let pkcs8_bytes = match &pkcs8 {
        PrivateKeyDer::Pkcs8(p) => p.secret_pkcs8_der(),
        _ => unreachable!(),
    };
    let info = pkcs8::PrivateKeyInfoRef::try_from(pkcs8_bytes).expect("PKCS#8");
    let inner = sec1::EcPrivateKey::try_from(info.private_key.as_bytes()).expect("inner SEC1");
    let original_pub = inner.public_key.expect("inner has publicKey");
    assert_eq!(original_pub[0], 0x04, "rcgen produces uncompressed");
    assert_eq!(original_pub.len(), 1 + 64);

    // Build a synthetic *compressed* publicKey of the right length
    // (33 bytes for P-256: 0x02/0x03 || X). The X-coordinate bytes
    // don't need to be the true encoding of a point on the curve —
    // we want to verify that our code refuses **before** any
    // cryptographic interpretation. The 0x02 prefix is the trigger.
    let mut compressed = vec![0x02u8];
    compressed.extend_from_slice(&original_pub[1..1 + 32]); // X half
    // Re-encode SEC1 with this synthetic compressed publicKey. The
    // sec1 crate stores publicKey as a raw byte slice, so the
    // re-encoded DER will faithfully preserve our compressed prefix.
    let modified = sec1::EcPrivateKey {
        private_key: inner.private_key,
        parameters: Some(EcParameters::NamedCurve(SECP256R1_OID)),
        public_key: Some(&compressed),
    };
    let der = modified.to_der().expect("re-encode SEC1");
    let bare = PrivateKeyDer::Sec1(PrivateSec1KeyDer::from(der));

    match EcSigningKey::new(&bare) {
        Err(Error::General(msg)) => {
            // Could be rejected on length (33 != 65) or on prefix
            // (0x02 not 0x04). Either is a valid fail-closed response.
            assert!(
                msg.contains("not uncompressed") || msg.contains("length"),
                "compressed publicKey rejection must reference uncompressed-prefix \
                 or length check, got {msg:?}"
            );
        }
        other => panic!("expected Error::General for compressed publicKey, got {other:?}"),
    }
}

/// PKCS#8 with the wrong algorithm OID (e.g. an RSA-wrapped key fed
/// to the EC parser directly) must be rejected. In normal flow the
/// dispatcher in `signer/mod.rs` tries RSA first, so this would only
/// be reached if a user called `EcSigningKey::new` directly with an
/// RSA PKCS#8 — but the assertion guards against accidentally
/// loosening the OID check in the future.
#[test]
fn pkcs8_with_non_ec_oid_is_rejected() {
    // Construct a minimal PKCS#8-wrapped RSA key (use rcgen's RSA
    // helper, which produces id-rsaEncryption-wrapped PKCS#8).
    use rcgen::{KeyPair, PKCS_RSA_SHA256};
    let kp = KeyPair::generate_for(&PKCS_RSA_SHA256).expect("rcgen RSA");
    let pem = kp.serialize_pem();
    let rsa_der = PrivateKeyDer::from_pem_slice(pem.as_bytes()).expect("decode PEM");

    // Feed RSA PKCS#8 directly to the EC constructor — should reject.
    match EcSigningKey::new(&rsa_der) {
        Err(Error::General(msg)) => assert!(
            msg.contains("not id-ecPublicKey"),
            "RSA PKCS#8 into EC parser must reference algorithm OID, got {msg:?}"
        ),
        other => panic!("expected Error::General for RSA PKCS#8, got {other:?}"),
    }
}

/// `EcSigner` Debug-impl smoke (manual impl, since `Algorithm` doesn't
/// derive Debug). Mirrors the rsa.rs sibling test.
#[test]
fn ec_signer_debug_smoke() {
    let der = gen_ec_key(&PKCS_ECDSA_P256_SHA256);
    let key = EcSigningKey::new(&der).expect("load");
    let signer = key
        .choose_scheme(&[SignatureScheme::ECDSA_NISTP256_SHA256])
        .expect("scheme");
    let s = format!("{signer:?}");
    assert!(s.contains("EcSigner"), "Debug output: {s}");
}

#[test]
fn choose_scheme_only_matches_paired_hash() {
    let der = gen_ec_key(&PKCS_ECDSA_P256_SHA256);
    let k = EcSigningKey::new(&der).expect("load");

    // The paired scheme is the only acceptable match.
    let signer = k
        .choose_scheme(&[SignatureScheme::ECDSA_NISTP256_SHA256])
        .expect("scheme");
    assert_eq!(signer.scheme(), SignatureScheme::ECDSA_NISTP256_SHA256);

    // Offering only mismatched ECDSA schemes (e.g. P-384) gives None.
    assert!(
        k.choose_scheme(&[SignatureScheme::ECDSA_NISTP384_SHA384])
            .is_none()
    );

    // Offering only RSA schemes also gives None.
    assert!(
        k.choose_scheme(&[SignatureScheme::RSA_PSS_SHA256])
            .is_none()
    );
}

fn roundtrip(alg: &'static rcgen::SignatureAlgorithm, curve: EcCurve) {
    let der = gen_ec_key(alg);
    let key = EcSigningKey::new(&der).expect("load");
    let signer = key.choose_scheme(&[curve.scheme()]).expect("scheme");
    let msg = format!("ec roundtrip {curve:?}");
    let sig = signer.sign(msg.as_bytes()).expect("sign");

    let pub_bytes = key
        .key
        .public_key()
        .expect("pub")
        .external_representation()
        .expect("ext")
        .bytes()
        .to_vec();
    let alg_for_verify = crate::verify::SUPPORTED_SIG_ALGS
        .mapping
        .iter()
        .find(|(s, _)| *s == curve.scheme())
        .and_then(|(_, algs)| algs.first())
        .expect("scheme in mapping");
    alg_for_verify
        .verify_signature(&pub_bytes, msg.as_bytes(), &sig)
        .expect("verify");
}

#[test]
fn roundtrip_p256_sha256() {
    roundtrip(&PKCS_ECDSA_P256_SHA256, EcCurve::P256);
}
#[test]
fn roundtrip_p384_sha384() {
    roundtrip(&PKCS_ECDSA_P384_SHA384, EcCurve::P384);
}
#[test]
fn roundtrip_p521_sha512() {
    roundtrip(&PKCS_ECDSA_P521_SHA512, EcCurve::P521);
}

/// **Test-gap #11.** A PKCS#8 wrapper carrying a non-P-curve OID
/// (e.g. secp256k1, the Bitcoin curve, OID `1.3.132.0.10`) must be
/// refused at load time with the documented "not P-256 / P-384 /
/// P-521" marker. We build the PKCS#8 by hand using a real P-256
/// scalar but stamping the algorithm.parameters with the secp256k1
/// OID — the EC parser must reject on the curve-OID check before
/// any cryptographic interpretation.
#[test]
fn ec_signing_key_rejects_non_p_curve_oid() {
    use pkcs8::der::{Decode as _, Encode as _};
    use rustls::pki_types::PrivatePkcs8KeyDer;

    // secp256k1 is the Bitcoin curve; not on our P-* whitelist.
    const SECP256K1_OID: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.132.0.10");

    // Build a real P-256 SEC1 ECPrivateKey (correct scalar + pub
    // point) so the *inner* parse succeeds — we want the rejection
    // to come from the outer curve-OID gate, not from a malformed
    // SEC1 blob.
    let pkcs8_p256 = gen_ec_key(&PKCS_ECDSA_P256_SHA256);
    let p256_bytes = match &pkcs8_p256 {
        PrivateKeyDer::Pkcs8(p) => p.secret_pkcs8_der(),
        _ => unreachable!(),
    };
    let info_p256 = pkcs8::PrivateKeyInfoRef::try_from(p256_bytes).expect("PKCS#8 P-256");
    let inner_p256 =
        sec1::EcPrivateKey::try_from(info_p256.private_key.as_bytes()).expect("inner SEC1 P-256");

    // Re-wrap that SEC1 under a PKCS#8 envelope whose
    // algorithm.parameters claim secp256k1. Use the `pkcs8` crate's
    // builder to produce a strictly-conformant envelope.
    let sec1_inner_der = sec1::EcPrivateKey {
        private_key: inner_p256.private_key,
        parameters: Some(sec1::EcParameters::NamedCurve(SECP256K1_OID)),
        public_key: inner_p256.public_key,
    }
    .to_der()
    .expect("encode inner SEC1 with secp256k1 OID");

    let secp256k1_oid_der = SECP256K1_OID.to_der().expect("encode OID");
    let alg = pkcs8::AlgorithmIdentifierRef {
        oid: ID_EC_PUBLIC_KEY,
        parameters: Some(
            pkcs8::der::AnyRef::from_der(&secp256k1_oid_der).expect("AnyRef from OID"),
        ),
    };
    let info = pkcs8::PrivateKeyInfoRef {
        algorithm: alg,
        private_key: pkcs8::der::asn1::OctetStringRef::new(&sec1_inner_der)
            .expect("wrap inner SEC1 in OCTET STRING"),
        public_key: None,
    };
    let pkcs8_der = info.to_der().expect("encode PKCS#8");

    let bogus = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(pkcs8_der));
    match EcSigningKey::new(&bogus) {
        Err(Error::General(msg)) => assert!(
            msg.contains("not P-256 / P-384 / P-521"),
            "non-P-curve OID must surface the documented marker, got {msg:?}"
        ),
        other => panic!("expected curve-OID rejection, got {other:?}"),
    }
}
