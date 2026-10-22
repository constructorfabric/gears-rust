use super::*;
use rcgen::{KeyPair, PKCS_RSA_SHA256};
use rustls::pki_types::pem::PemObject;

fn gen_rsa_key_der() -> PrivateKeyDer<'static> {
    let key_pair = KeyPair::generate_for(&PKCS_RSA_SHA256).expect("rcgen RSA");
    // rcgen produces PKCS#8 PEM by default.
    let pem = key_pair.serialize_pem();
    PrivateKeyDer::from_pem_slice(pem.as_bytes()).expect("decode PEM")
}

/// Convert a PKCS#8-wrapped key to a `PrivateKeyDer::Pkcs1` by stripping
/// the outer envelope. Lets us cover the `PrivateKeyDer::Pkcs1` arm in
/// `RsaSigningKey::new` — rcgen always emits PKCS#8 PEM, so without this
/// helper that branch stays dark.
fn pkcs8_to_pkcs1_rsa(pkcs8: &PrivateKeyDer<'_>) -> PrivateKeyDer<'static> {
    use rustls::pki_types::PrivatePkcs1KeyDer;
    let bytes = match pkcs8 {
        PrivateKeyDer::Pkcs8(p) => p.secret_pkcs8_der(),
        other => panic!("expected PKCS#8 input, got {other:?}"),
    };
    let info = pkcs8::PrivateKeyInfoRef::try_from(bytes).expect("parse PKCS#8");
    PrivateKeyDer::Pkcs1(PrivatePkcs1KeyDer::from(
        info.private_key.as_bytes().to_vec(),
    ))
}

/// Loading + immediate usability: after `new`, the key must offer at
/// least one scheme from `RSA_SCHEMES`. Stronger than a bare load —
/// catches the case where SecKeyCreateWithData accepts the bytes but
/// the resulting key cannot be exercised (e.g. wrong KeyClass).
#[test]
fn loads_pkcs8_rsa_and_is_usable() {
    let der = gen_rsa_key_der();
    let key = RsaSigningKey::new(&der).expect("load PKCS#8 RSA");
    let signer = key.choose_scheme(RSA_SCHEMES).expect("usable key");
    // Smoke: signer can produce something, not just exist.
    let sig = signer.sign(b"smoke").expect("sign");
    assert!(!sig.is_empty(), "RSA signature must not be empty");
}

/// PKCS#1 path: rcgen emits PKCS#8 but some operators ship PKCS#1
/// directly. Strip the wrapper ourselves and feed bare PKCS#1 to
/// `new`, then verify a real signature roundtrip — proves the
/// `PrivateKeyDer::Pkcs1` arm in `new` actually works end-to-end.
#[test]
fn loads_pkcs1_rsa_roundtrip() {
    let pkcs8 = gen_rsa_key_der();
    let pkcs1 = pkcs8_to_pkcs1_rsa(&pkcs8);
    let key = RsaSigningKey::new(&pkcs1).expect("load PKCS#1 RSA");
    let signer = key
        .choose_scheme(&[SignatureScheme::RSA_PSS_SHA256])
        .expect("scheme");
    let msg = b"pkcs1 roundtrip";
    let sig = signer.sign(msg).expect("sign");
    // Verify through our public-side path to prove wire-format agreement.
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
        .find(|(s, _)| *s == SignatureScheme::RSA_PSS_SHA256)
        .and_then(|(_, a)| a.first())
        .expect("scheme in mapping")
        .verify_signature(&pub_bytes, msg, &sig)
        .expect("verify PKCS#1-loaded signature");
}

/// Wrong-encoding rejection: SEC1 is for EC keys, must be refused by
/// the RSA constructor with a documented marker string. The dispatcher
/// in `signer/mod.rs` then falls through to the EC constructor.
#[test]
fn rejects_sec1_input_as_not_rsa() {
    use rustls::pki_types::PrivateSec1KeyDer;
    let bogus = PrivateKeyDer::Sec1(PrivateSec1KeyDer::from(vec![0u8; 32]));
    match RsaSigningKey::new(&bogus) {
        Err(Error::General(msg)) => assert!(
            msg.contains("SEC1 is an EC encoding"),
            "error must explain SEC1 mismatch, got {msg:?}"
        ),
        other => panic!("expected Error::General for SEC1, got {other:?}"),
    }
}

/// `RsaSigner` must impl `Debug` without panicking — required by
/// rustls's `Signer: Debug` bound. Trivial but the manual impl is
/// hand-rolled (`Algorithm` doesn't derive Debug), so a smoke test
/// guards against future fmt-impl regressions.
#[test]
fn rsa_signer_debug_smoke() {
    let der = gen_rsa_key_der();
    let key = RsaSigningKey::new(&der).expect("load");
    let signer = key.choose_scheme(RSA_SCHEMES).expect("scheme");
    let s = format!("{signer:?}");
    assert!(s.contains("RsaSigner"), "Debug output: {s}");
}

/// Contract: every scheme in `RSA_SCHEMES` must have a non-panicking
/// mapping in `scheme_to_algorithm`. A regression that extends
/// `RSA_SCHEMES` without extending the match arm would surface here
/// rather than at signing time.
#[test]
fn scheme_to_algorithm_total_over_rsa_schemes() {
    for &scheme in RSA_SCHEMES {
        // Must not panic — `Algorithm` variants are private to
        // `security_framework`, so we only assert that the call
        // returns without unwinding.
        let _ = scheme_to_algorithm(scheme);
    }
}

#[test]
fn choose_scheme_picks_pss512_when_offered_all() {
    let der = gen_rsa_key_der();
    let key = RsaSigningKey::new(&der).expect("load");
    let offered: Vec<SignatureScheme> = RSA_SCHEMES.iter().rev().copied().collect();
    let signer = key.choose_scheme(&offered).expect("scheme");
    assert_eq!(signer.scheme(), SignatureScheme::RSA_PSS_SHA512);
}

#[test]
fn choose_scheme_returns_none_when_no_offered_overlap() {
    let der = gen_rsa_key_der();
    let key = RsaSigningKey::new(&der).expect("load");
    // Offer only ECDSA schemes; RSA key cannot satisfy them.
    let offered = [
        SignatureScheme::ECDSA_NISTP256_SHA256,
        SignatureScheme::ECDSA_NISTP384_SHA384,
    ];
    assert!(key.choose_scheme(&offered).is_none());
}

/// End-to-end roundtrip per scheme: sign with our `Signer`, verify with
/// our own `verify::SUPPORTED_SIG_ALGS` chain. This proves both halves
/// agree on the wire format, which is the only thing rustls relies on.
fn roundtrip(scheme: SignatureScheme) {
    let der = gen_rsa_key_der();
    let key = RsaSigningKey::new(&der).expect("load");
    let signer = key.choose_scheme(&[scheme]).expect("scheme");
    let msg = format!("rsa roundtrip {scheme:?}");
    let sig = signer.sign(msg.as_bytes()).expect("sign");

    // Verify via our public-side path.
    let pub_bytes = key
        .key
        .public_key()
        .expect("pub")
        .external_representation()
        .expect("ext")
        .bytes()
        .to_vec();
    let alg = crate::verify::SUPPORTED_SIG_ALGS
        .mapping
        .iter()
        .find(|(s, _)| *s == scheme)
        .and_then(|(_, algs)| algs.first())
        .expect("scheme in mapping");
    alg.verify_signature(&pub_bytes, msg.as_bytes(), &sig)
        .expect("verify");
}

#[test]
fn roundtrip_rsa_pss_sha256() {
    roundtrip(SignatureScheme::RSA_PSS_SHA256);
}
#[test]
fn roundtrip_rsa_pss_sha384() {
    roundtrip(SignatureScheme::RSA_PSS_SHA384);
}
#[test]
fn roundtrip_rsa_pss_sha512() {
    roundtrip(SignatureScheme::RSA_PSS_SHA512);
}
#[test]
fn roundtrip_rsa_pkcs1_sha256() {
    roundtrip(SignatureScheme::RSA_PKCS1_SHA256);
}
#[test]
fn roundtrip_rsa_pkcs1_sha384() {
    roundtrip(SignatureScheme::RSA_PKCS1_SHA384);
}
#[test]
fn roundtrip_rsa_pkcs1_sha512() {
    roundtrip(SignatureScheme::RSA_PKCS1_SHA512);
}

/// FIPS-4: RSA modulus < 2048 bits must be rejected at load time.
/// Apple's `SecKeyCreateWithData` happily accepts 1024-bit RSA keys
/// (Apple itself imposes no FIPS-mode restriction on the loader),
/// so the check has to live in our code. Loading rcgen's smallest
/// RSA key would be wasteful — generate a 1024-bit key directly via
/// `SecKey::new` and re-encode its private bytes into a fake
/// `PrivateKeyDer::Pkcs1` to drive the rejection path.
#[test]
fn rsa_1024_key_is_rejected() {
    use crate::ffi::security::seckey_block_size;
    use rustls::pki_types::PrivatePkcs1KeyDer;
    use security_framework::key::{GenerateKeyOptions, KeyType, SecKey};

    // Mint an Apple-side RSA-1024 key, extract its PKCS#1 private
    // bytes via Apple's external representation (`SecKeyCopy*` family
    // returns PKCS#1 for RSA), then feed those bytes back to
    // `RsaSigningKey::new` and expect the FIPS-4 gate to fire.
    let mut opts = GenerateKeyOptions::default();
    opts.set_key_type(KeyType::rsa());
    opts.set_size_in_bits(1024);
    let weak = SecKey::new(&opts).expect("apple 1024-bit RSA keygen");
    assert_eq!(
        seckey_block_size(&weak),
        128,
        "1024-bit modulus = 128 bytes"
    );
    let pkcs1_bytes = weak
        .external_representation()
        .expect("external_representation")
        .bytes()
        .to_vec();
    let der = PrivateKeyDer::Pkcs1(PrivatePkcs1KeyDer::from(pkcs1_bytes));

    match RsaSigningKey::new(&der) {
        Err(Error::General(msg)) => assert!(
            msg.contains("below the FIPS 186-5 minimum of 2048") && msg.contains("1024-bit"),
            "FIPS-4 rejection must reference 2048-bit minimum and the actual key size, got {msg:?}"
        ),
        Err(other) => panic!("expected General error, got {other:?}"),
        Ok(_) => panic!("RSA-1024 was accepted; FIPS-4 gate is broken"),
    }
}

/// **TODO-4 / PSS salt-length regression-watch.** Apple's `SecKey.h`
/// documents `kSecKeyAlgorithmRSASignatureMessagePSSSHA{256,384,512}`
/// with `saltLength = 32/48/64` (= digest length), matching RFC 8446
/// §4.2.3 and RFC 8017 §9.1. If Apple ever silently changes this in
/// a future macOS SDK release, TLS 1.3 interop would break for any
/// RFC-compliant peer and the FIPS-claim correctness would be lost.
///
/// We catch the drift via **cross-implementation verification**: sign
/// with Apple's path (our `RsaSigner`), then verify with the
/// pure-Rust `rsa` crate using `Pss::new_with_salt::<D>(hash_len)` —
/// which only accepts signatures whose salt is exactly `hash_len`
/// bytes (the RFC-mandated value). A mismatch surfaces as a verify
/// failure and fails this test.
///
/// Coverage: all three SHA variants. Single test runs all three so a
/// silent breakage of any one is caught.
#[test]
fn rsa_pss_apple_salt_length_matches_rfc8017() {
    use rsa::RsaPublicKey;
    use rsa::pkcs1::DecodeRsaPublicKey;
    use rsa::pss::Pss;
    use sha2::{Digest, Sha256, Sha384, Sha512};

    let der = gen_rsa_key_der();
    let key = RsaSigningKey::new(&der).expect("load");

    // Apple's `external_representation` for RSA returns PKCS#1
    // RSAPublicKey DER (`SEQUENCE { modulus, publicExponent }`).
    let pub_bytes = key
        .key
        .public_key()
        .expect("pub")
        .external_representation()
        .expect("ext")
        .bytes()
        .to_vec();
    let pub_key = RsaPublicKey::from_pkcs1_der(&pub_bytes)
        .expect("parse Apple RSAPublicKey DER via rsa crate");

    // SHA-256 path: salt_len MUST be 32.
    {
        let signer = key
            .choose_scheme(&[SignatureScheme::RSA_PSS_SHA256])
            .expect("scheme");
        let msg = b"pss-salt-256-regression";
        let sig = signer.sign(msg).expect("sign");
        let hashed: [u8; 32] = Sha256::digest(msg).into();
        pub_key
            .verify(Pss::new_with_salt::<Sha256>(32), &hashed, &sig)
            .expect(
                "Apple PSS-SHA256 signature failed pure-Rust verify with \
                 salt_len=32 — Apple has silently changed its PSS salt-length \
                 default; TLS 1.3 interop and FIPS-claim correctness are broken",
            );
    }

    // SHA-384 path: salt_len MUST be 48.
    {
        let signer = key
            .choose_scheme(&[SignatureScheme::RSA_PSS_SHA384])
            .expect("scheme");
        let msg = b"pss-salt-384-regression";
        let sig = signer.sign(msg).expect("sign");
        let hashed = Sha384::digest(msg);
        pub_key
            .verify(Pss::new_with_salt::<Sha384>(48), &hashed, &sig)
            .expect(
                "Apple PSS-SHA384 signature failed pure-Rust verify with \
                 salt_len=48 — see SHA-256 case for impact",
            );
    }

    // SHA-512 path: salt_len MUST be 64.
    {
        let signer = key
            .choose_scheme(&[SignatureScheme::RSA_PSS_SHA512])
            .expect("scheme");
        let msg = b"pss-salt-512-regression";
        let sig = signer.sign(msg).expect("sign");
        let hashed = Sha512::digest(msg);
        pub_key
            .verify(Pss::new_with_salt::<Sha512>(64), &hashed, &sig)
            .expect(
                "Apple PSS-SHA512 signature failed pure-Rust verify with \
                 salt_len=64 — see SHA-256 case for impact",
            );
    }
}

/// **Test-gap #12.** RSA-4096 must be accepted (no upper modulus
/// ceiling beyond what Apple itself enforces). Documents by positive
/// example that the 2048-bit floor is a floor, not a single allowed
/// size. Uses Apple's `SecKey::new` directly so we don't have to
/// shell out to rcgen for a 4096-bit key (rcgen's RSA helper does
/// not let us pick a size).
#[test]
fn rsa_4096_key_is_accepted_and_signs() {
    use crate::ffi::security::seckey_block_size;
    use rustls::pki_types::PrivatePkcs1KeyDer;
    use security_framework::key::{GenerateKeyOptions, KeyType, SecKey};

    let mut opts = GenerateKeyOptions::default();
    opts.set_key_type(KeyType::rsa());
    opts.set_size_in_bits(4096);
    let strong = SecKey::new(&opts).expect("apple 4096-bit RSA keygen");
    assert_eq!(
        seckey_block_size(&strong),
        512,
        "4096-bit modulus = 512 bytes"
    );

    let pkcs1_bytes = strong
        .external_representation()
        .expect("external_representation")
        .bytes()
        .to_vec();
    let der = PrivateKeyDer::Pkcs1(PrivatePkcs1KeyDer::from(pkcs1_bytes));

    let key = RsaSigningKey::new(&der).expect("RSA-4096 must be accepted");
    let signer = key
        .choose_scheme(&[SignatureScheme::RSA_PSS_SHA256])
        .expect("scheme");
    let sig = signer.sign(b"4096-smoke").expect("sign with RSA-4096");
    // RSA-4096 signature is 512 bytes (modulus size).
    assert_eq!(sig.len(), 512, "RSA-4096 signature must be modulus-sized");
}

/// Concurrent signing with one `Arc<SigningKey>`: 16 threads × 64
/// independent sign() calls each, all produce verify-able signatures.
/// This is the contract rustls relies on -- `SigningKey` is wrapped in
/// `Arc<dyn SigningKey>` and rustls hands clones to multiple handler
/// threads. `RsaSigningKey` derives `Send + Sync` via auto-traits
/// on `Arc<SecKey>` (SecKey itself carries upstream
/// `unsafe impl Send + Sync` in security-framework); if either of
/// those upstream contracts were unsound, a TSan/UBSan run would
/// catch it here.
#[test]
fn concurrent_sign_with_shared_key() {
    use std::sync::Arc;
    use std::thread;

    let der = gen_rsa_key_der();
    // Keep a typed `Arc<RsaSigningKey>` for the test-side public-key
    // extraction; clone into `Arc<dyn SigningKey>` for the rustls
    // contract surface used by the threads. This avoids casting a
    // wide trait-object pointer to a thin concrete pointer, which
    // would rely on an unspecified Rust ABI invariant.
    let typed = Arc::new(RsaSigningKey::new(&der).expect("load"));
    let pub_bytes = typed
        .key
        .public_key()
        .expect("pub")
        .external_representation()
        .expect("ext")
        .bytes()
        .to_vec();
    let key: Arc<dyn SigningKey> = typed.clone();
    assert_eq!(key.algorithm(), SignatureAlgorithm::RSA);

    let mut handles = vec![];
    for t in 0..16 {
        let key = Arc::clone(&key);
        let pub_bytes = pub_bytes.clone();
        handles.push(thread::spawn(move || {
            for i in 0..64 {
                let signer = key
                    .choose_scheme(&[SignatureScheme::RSA_PSS_SHA256])
                    .expect("scheme");
                let msg = format!("thread {t} iteration {i}");
                let sig = signer.sign(msg.as_bytes()).expect("sign");
                assert!(!sig.is_empty());
                // Verify wire-correctness — sig must verify against
                // our own verify path with the matching pub key.
                let alg = crate::verify::SUPPORTED_SIG_ALGS
                    .mapping
                    .iter()
                    .find(|(s, _)| *s == SignatureScheme::RSA_PSS_SHA256)
                    .and_then(|(_, a)| a.first())
                    .expect("scheme");
                alg.verify_signature(&pub_bytes, msg.as_bytes(), &sig)
                    .expect("concurrent verify");
            }
        }));
    }
    for h in handles {
        h.join().expect("thread completed without panic");
    }
}
