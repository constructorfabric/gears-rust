//! RSA `SigningKey` over Apple corecrypto.
//!
//! Supports RSA-PSS and RSA-PKCS#1 v1.5 with SHA-256 / SHA-384 / SHA-512,
//! parity with [`rustls-cng-crypto`'s `signer/rsa.rs`
//! ](https://docs.rs/rustls-cng-crypto/0.1.2/rustls_cng_crypto/) on the
//! Windows side.
//!
//! ## Key import flow
//!
//! Apple's `SecKeyCreateWithData` accepts **PKCS#1 `RSAPrivateKey` DER**
//! (the inner SEQUENCE) but **rejects PKCS#8** envelopes outright. rustls
//! delivers private keys as `PrivateKeyDer::{Pkcs1, Pkcs8, Sec1}`; here
//! we accept:
//!
//! - `Pkcs1` — pass-through (already the format Apple wants).
//! - `Pkcs8` — unwrap via [`pkcs8::PrivateKeyInfo`], verify `algorithm.oid
//!   == rsaEncryption`, hand `.private_key` (the inner PKCS#1 bytes) to
//!   Apple.
//! - `Sec1` — rejected (EC-only encoding).
//!
//! The unwrap is a structural DER parse only — no cryptographic primitive
//! runs through the `pkcs8` crate, so the FIPS-claim chain-of-trust stays
//! anchored at corecrypto. Apple's
//! `kSecKeyAlgorithmRSASignatureMessage{Pss,Pkcs1v15}Sha{256,384,512}`
//! variants take the raw message and hash + sign inside corecrypto.

use std::sync::Arc;

use rustls::Error;
use rustls::SignatureAlgorithm;
use rustls::SignatureScheme;
use rustls::pki_types::PrivateKeyDer;
use rustls::sign::{Signer, SigningKey};
use security_framework::key::{Algorithm, SecKey};
use zeroize::Zeroizing;

use crate::ffi::security::{PrivateKeyKind, import_private_key, seckey_block_size};

/// RSA signature schemes we offer for negotiation, in descending order of
/// preference. Mirrors `rustls-cng-crypto::signer::rsa::RSA_SCHEMES`; the
/// constant is consumed by TLS 1.2 ECDHE_RSA cipher suites to advertise
/// supported `signature_algorithms` extension entries.
///
/// **Ordering rationale (A-3 comment per security review).** PSS is
/// listed before PKCS#1 v1.5 because:
///
/// 1. **TLS 1.3 mandates PSS** for `CertificateVerify` (RFC 8446 §4.2.3).
///    PKCS#1 v1.5 entries are disallowed for TLS 1.3 signing — rustls
///    filters them out of the TLS 1.3 sig_alg negotiation surface — so
///    in TLS 1.3 only the PSS half of this list is reachable.
/// 2. **PSS is the modern provable-security choice** (RSA-PSS has tight
///    reductions to RSA assumption; PKCS#1 v1.5 has a long history of
///    padding-oracle and Bleichenbacher-style attacks at the protocol
///    layer).
/// 3. **TLS 1.2 peer-compat keeps PKCS#1 v1.5** in the list — older
///    endpoints (and some embedded stacks) still negotiate it. Ordering
///    means we *prefer* PSS but accept PKCS#1 v1.5 as fallback when the
///    peer offers nothing else.
///
/// Within each family, SHA-512 → SHA-384 → SHA-256 prefers the larger
/// digest (parity with `rustls-cng-crypto` and `aws-lc-rs`).
pub(crate) static RSA_SCHEMES: &[SignatureScheme] = &[
    SignatureScheme::RSA_PSS_SHA512,
    SignatureScheme::RSA_PSS_SHA384,
    SignatureScheme::RSA_PSS_SHA256,
    SignatureScheme::RSA_PKCS1_SHA512,
    SignatureScheme::RSA_PKCS1_SHA384,
    SignatureScheme::RSA_PKCS1_SHA256,
];

/// RSA private key wrapped as an opaque `SecKey`. The DER bytes used to
/// construct it never leave the local stack — `SecKey` is reference-
/// counted by Apple corecrypto, so the secret material lives inside the
/// FIPS-validated module after `SecKeyCreateWithData` returns.
#[derive(Debug)]
pub(crate) struct RsaSigningKey {
    key: Arc<SecKey>,
}

impl RsaSigningKey {
    /// Construct an `RsaSigningKey` from a rustls `PrivateKeyDer`.
    ///
    /// Returns `Err` on non-RSA input or malformed DER. The error message
    /// is intentionally terse so the dispatcher in
    /// [`super::any_supported_type`] can silently fall through to EC.
    ///
    /// The intermediate PKCS#1 bytes are kept in a [`Zeroizing`] buffer so
    /// the plaintext private material is wiped from heap memory the
    /// moment `import_private_key` finishes (FIPS 140-3 IG 9.5 "no
    /// plaintext CSPs outside the cryptographic boundary").
    pub(crate) fn new(der: &PrivateKeyDer<'_>) -> Result<Self, Error> {
        let pkcs1: Zeroizing<Vec<u8>> = match der {
            PrivateKeyDer::Pkcs1(p) => Zeroizing::new(p.secret_pkcs1_der().to_vec()),
            PrivateKeyDer::Pkcs8(p) => extract_rsa_pkcs1_from_pkcs8(p.secret_pkcs8_der())?,
            PrivateKeyDer::Sec1(_) => {
                return Err(Error::General(
                    "rustls-corecrypto-provider: SEC1 is an EC encoding, not RSA".to_owned(),
                ));
            }
            _ => {
                return Err(Error::General(
                    "rustls-corecrypto-provider: unrecognized PrivateKeyDer variant".to_owned(),
                ));
            }
        };

        let key = import_private_key(&pkcs1, PrivateKeyKind::RsaPkcs1)
            .map_err(|e| Error::General(format!("RSA key import failed: {e}")))?;

        // FIPS-4: enforce minimum modulus size 2048 bits.
        // SecKeyGetBlockSize returns the signature length in bytes, which
        // equals the modulus size in bytes for RSA. NIST FIPS 186-5 §5.1
        // mandates RSA modulus ≥ 2048 bits for signing.
        let modulus_bytes = seckey_block_size(&key);
        if modulus_bytes < 256 {
            return Err(Error::General(format!(
                "RSA key modulus {}-bit is below the FIPS 186-5 minimum of 2048 bits",
                modulus_bytes * 8
            )));
        }

        Ok(Self { key: Arc::new(key) })
    }
}

impl SigningKey for RsaSigningKey {
    fn choose_scheme(&self, offered: &[SignatureScheme]) -> Option<Box<dyn Signer>> {
        // Iterate by *our* preference order, not the peer's — matches what
        // rustls-cng-crypto does and what aws-lc-rs's `RsaSigningKey` does.
        // The peer's `offered` list is a filter, not a ranking.
        RSA_SCHEMES
            .iter()
            .find(|scheme| offered.contains(scheme))
            .map(|scheme| {
                Box::new(RsaSigner {
                    key: Arc::clone(&self.key),
                    scheme: *scheme,
                    algorithm: scheme_to_algorithm(*scheme),
                }) as Box<dyn Signer>
            })
    }

    fn algorithm(&self) -> SignatureAlgorithm {
        SignatureAlgorithm::RSA
    }
}

struct RsaSigner {
    key: Arc<SecKey>,
    scheme: SignatureScheme,
    algorithm: Algorithm,
}

// `security_framework::key::Algorithm` does not implement `Debug` upstream,
// so the derive macro can't reach this struct. We only need a Debug to
// satisfy rustls's `Signer: Debug` bound; the algorithm is captured
// indirectly via `scheme` for human inspection.
impl std::fmt::Debug for RsaSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RsaSigner")
            .field("scheme", &self.scheme)
            .finish_non_exhaustive()
    }
}

impl Signer for RsaSigner {
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, Error> {
        self.key
            .create_signature(self.algorithm, message)
            .map_err(|e| {
                // Only structured fields — avoid `{e:?}` which includes
                // Apple's localized description.
                Error::General(format!(
                    "RSA sign failed: domain={} code={}",
                    e.domain(),
                    e.code()
                ))
            })
    }

    fn scheme(&self) -> SignatureScheme {
        self.scheme
    }
}

/// Map a TLS-level `SignatureScheme` to the corresponding Apple corecrypto
/// `SecKeyAlgorithm` (via the `security-framework` enum). The mapping is
/// total over `RSA_SCHEMES`, so callers that picked a scheme from there
/// can rely on the `unreachable!` arm staying unreached.
///
/// **Invariant (covered by `scheme_to_algorithm_total_over_rsa_schemes`
/// test): every entry in `RSA_SCHEMES` must map to a real `Algorithm`,
/// not panic.** A regression that added a new scheme to `RSA_SCHEMES`
/// without extending this `match` would surface as a test failure
/// rather than a runtime panic on the first signing operation.
fn scheme_to_algorithm(scheme: SignatureScheme) -> Algorithm {
    match scheme {
        SignatureScheme::RSA_PSS_SHA256 => Algorithm::RSASignatureMessagePSSSHA256,
        SignatureScheme::RSA_PSS_SHA384 => Algorithm::RSASignatureMessagePSSSHA384,
        SignatureScheme::RSA_PSS_SHA512 => Algorithm::RSASignatureMessagePSSSHA512,
        SignatureScheme::RSA_PKCS1_SHA256 => Algorithm::RSASignatureMessagePKCS1v15SHA256,
        SignatureScheme::RSA_PKCS1_SHA384 => Algorithm::RSASignatureMessagePKCS1v15SHA384,
        SignatureScheme::RSA_PKCS1_SHA512 => Algorithm::RSASignatureMessagePKCS1v15SHA512,
        other => unreachable!(
            "scheme_to_algorithm called with non-RSA scheme {other:?}; \
             choose_scheme should have filtered via RSA_SCHEMES"
        ),
    }
}

/// PKCS#1 RSA OID per RFC 3279 §2.3.1.
const RSA_ENCRYPTION_OID: pkcs8::ObjectIdentifier =
    pkcs8::ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.1");

/// Strip a PKCS#8 envelope and return the inner PKCS#1 `RSAPrivateKey`
/// bytes in a [`Zeroizing`] buffer. Rejects PKCS#8 wrappers whose
/// algorithm OID is not rsaEncryption (e.g. an EC key would be rejected
/// here and the dispatcher would then fall through to the EC path).
fn extract_rsa_pkcs1_from_pkcs8(pkcs8_der: &[u8]) -> Result<Zeroizing<Vec<u8>>, Error> {
    let info = pkcs8::PrivateKeyInfoRef::try_from(pkcs8_der)
        .map_err(|e| Error::General(format!("PKCS#8 parse failed: {e}")))?;
    if info.algorithm.oid != RSA_ENCRYPTION_OID {
        return Err(Error::General(format!(
            "PKCS#8 algorithm OID is not rsaEncryption: got {}",
            info.algorithm.oid
        )));
    }
    Ok(Zeroizing::new(info.private_key.as_bytes().to_vec()))
}

#[cfg(test)]
#[path = "rsa_tests.rs"]
mod tests;
