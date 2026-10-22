//! ECDSA `SigningKey` over Apple corecrypto.
//!
//! Supports NIST P-256, P-384, P-521 each paired with the matching SHA-2
//! hash, parity with [`rustls-cng-crypto`'s `signer/ec.rs`
//! ](https://docs.rs/rustls-cng-crypto/0.1.2/rustls_cng_crypto/) on the
//! Windows side.
//!
//! ## Key import flow — FIPS-aware
//!
//! Apple's `SecKeyCreateWithData` for EC private keys expects an **ANSI
//! X9.63 raw blob**: `0x04 || X || Y || k` (uncompressed public point
//! followed by the private scalar). This is **neither SEC1 nor PKCS#8** —
//! both must be unwrapped before calling Apple.
//!
//! Critically: we **must NOT derive the public point `Q = d · G` from the
//! private scalar `d` outside Apple corecrypto** — that would be an EC
//! point-multiplication on the private key, performed by non-validated
//! Rust code, which violates the FIPS 140-3 cryptographic boundary.
//! Instead, we read `Q` from the `publicKey BIT STRING` OPTIONAL field of
//! SEC1's `EcPrivateKey` (RFC 5915 §3), which standard tooling (rcgen,
//! OpenSSL) always embeds. This is the same posture
//! [`rustls-cng-crypto`'s ec.rs
//! ](https://github.com/tofay/rustls-cng-crypto/blob/main/src/signer/ec.rs)
//! takes on Windows.
//!
//! rustls delivers private keys as `PrivateKeyDer::{Pkcs1, Pkcs8, Sec1}`;
//! here we accept:
//!
//! - `Pkcs8` — parse via [`pkcs8::PrivateKeyInfo`], verify
//!   `algorithm.oid == id-ecPublicKey`, read the curve OID from
//!   `algorithm.parameters`, then parse the inner OCTET STRING as
//!   [`sec1::EcPrivateKey`] and extract the embedded `publicKey`.
//! - `Sec1` — parse directly as `sec1::EcPrivateKey`; curve OID comes
//!   from `parameters` (or from the PKCS#8 wrapper if we got here via the
//!   PKCS#8 path).
//! - `Pkcs1` — rejected (RSA-only encoding).
//!
//! Fail-closed cases:
//! - SEC1 has no embedded `publicKey` field → `Err(...)` with a marker
//!   message telling the operator to provide a key with the public point
//!   embedded (or to use a Keychain-stored key flow, future enhancement).
//! - Curve OID is not P-256 / P-384 / P-521 → reject.
//! - `publicKey` is compressed (`0x02`/`0x03` prefix) → reject (we only
//!   support uncompressed for X9.63).
//!
//! The `pkcs8` / `sec1` crates perform **only structural DER parsing** —
//! no curve arithmetic, no cryptographic primitives. Every cryptographic
//! operation (signing, hashing, public-key derivation never needed)
//! happens inside Apple corecrypto, preserving the CMVP chain-of-trust.

use std::sync::Arc;

use pkcs8::{ObjectIdentifier, PrivateKeyInfoRef};
use rustls::Error;
use rustls::SignatureAlgorithm;
use rustls::SignatureScheme;
use rustls::pki_types::PrivateKeyDer;
use rustls::sign::{Signer, SigningKey};
use sec1::EcPrivateKey;
use security_framework::key::{Algorithm, SecKey};
use zeroize::Zeroizing;

use crate::ffi::security::{PrivateKeyKind, import_private_key};

/// `id-ecPublicKey` (RFC 5480 §2.1.1).
const ID_EC_PUBLIC_KEY: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");

/// NIST P-256 / secp256r1 named curve OID.
const SECP256R1_OID: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.3.1.7");
/// NIST P-384 / secp384r1 named curve OID.
const SECP384R1_OID: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.132.0.34");
/// NIST P-521 / secp521r1 named curve OID.
const SECP521R1_OID: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.132.0.35");

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum EcCurve {
    P256,
    P384,
    P521,
}

impl EcCurve {
    fn private_key_kind(self) -> PrivateKeyKind {
        match self {
            Self::P256 => PrivateKeyKind::EcSecPrimeRandomP256,
            Self::P384 => PrivateKeyKind::EcSecPrimeRandomP384,
            Self::P521 => PrivateKeyKind::EcSecPrimeRandomP521,
        }
    }

    fn scheme(self) -> SignatureScheme {
        match self {
            Self::P256 => SignatureScheme::ECDSA_NISTP256_SHA256,
            Self::P384 => SignatureScheme::ECDSA_NISTP384_SHA384,
            Self::P521 => SignatureScheme::ECDSA_NISTP521_SHA512,
        }
    }

    fn algorithm(self) -> Algorithm {
        match self {
            Self::P256 => Algorithm::ECDSASignatureMessageX962SHA256,
            Self::P384 => Algorithm::ECDSASignatureMessageX962SHA384,
            Self::P521 => Algorithm::ECDSASignatureMessageX962SHA512,
        }
    }

    /// Bytes per coordinate (X, Y) and per private scalar. P-521 has
    /// 521-bit values that are encoded as 66 bytes (ceil(521 / 8)).
    fn coord_bytes(self) -> usize {
        match self {
            Self::P256 => 32,
            Self::P384 => 48,
            Self::P521 => 66,
        }
    }

    fn from_oid(oid: &ObjectIdentifier) -> Option<Self> {
        if *oid == SECP256R1_OID {
            Some(Self::P256)
        } else if *oid == SECP384R1_OID {
            Some(Self::P384)
        } else if *oid == SECP521R1_OID {
            Some(Self::P521)
        } else {
            None
        }
    }
}

/// ECDSA private key wrapped as an opaque `SecKey` plus the curve tag
/// (needed at sign time to pick the matching scheme + algorithm).
#[derive(Debug)]
pub(crate) struct EcSigningKey {
    key: Arc<SecKey>,
    curve: EcCurve,
}

impl EcSigningKey {
    pub(crate) fn new(der: &PrivateKeyDer<'_>) -> Result<Self, Error> {
        // The X9.63 blob `0x04 || X || Y || k` contains the private scalar,
        // so it is held in a `Zeroizing<Vec<u8>>` and wiped from heap memory
        // when this scope drops it after `import_private_key` finishes.
        let (blob, curve): (Zeroizing<Vec<u8>>, EcCurve) = match der {
            PrivateKeyDer::Pkcs8(p) => extract_x963_from_pkcs8(p.secret_pkcs8_der())?,
            PrivateKeyDer::Sec1(p) => extract_x963_from_sec1(p.secret_sec1_der())?,
            PrivateKeyDer::Pkcs1(_) => {
                return Err(Error::General(
                    "rustls-corecrypto-provider: PKCS#1 is an RSA encoding, not EC".to_owned(),
                ));
            }
            _ => {
                return Err(Error::General(
                    "rustls-corecrypto-provider: unrecognized PrivateKeyDer variant".to_owned(),
                ));
            }
        };

        let key = import_private_key(&blob, curve.private_key_kind())
            .map_err(|e| Error::General(format!("EC key import failed: {e}")))?;

        Ok(Self {
            key: Arc::new(key),
            curve,
        })
    }
}

impl SigningKey for EcSigningKey {
    fn choose_scheme(&self, offered: &[SignatureScheme]) -> Option<Box<dyn Signer>> {
        let scheme = self.curve.scheme();
        if !offered.contains(&scheme) {
            return None;
        }
        Some(Box::new(EcSigner {
            key: Arc::clone(&self.key),
            scheme,
            algorithm: self.curve.algorithm(),
        }) as Box<dyn Signer>)
    }

    fn algorithm(&self) -> SignatureAlgorithm {
        SignatureAlgorithm::ECDSA
    }
}

struct EcSigner {
    key: Arc<SecKey>,
    scheme: SignatureScheme,
    algorithm: Algorithm,
}

// See `RsaSigner` for why Debug is hand-rolled rather than derived.
impl std::fmt::Debug for EcSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EcSigner")
            .field("scheme", &self.scheme)
            .finish_non_exhaustive()
    }
}

impl Signer for EcSigner {
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, Error> {
        // Apple's `ECDSASignatureMessageX962SHA*` returns ASN.1
        // DER-encoded `SEQUENCE { r INTEGER, s INTEGER }` — already in
        // the wire format rustls expects, no P1363→DER conversion needed.
        self.key
            .create_signature(self.algorithm, message)
            .map_err(|e| {
                Error::General(format!(
                    "EC sign failed: domain={} code={}",
                    e.domain(),
                    e.code()
                ))
            })
    }

    fn scheme(&self) -> SignatureScheme {
        self.scheme
    }
}

// =========================================================================
// PKCS#8 / SEC1 → ANSI X9.63 conversion via *structural* DER parsing only.
//
// No curve arithmetic happens in this module. The public point is taken
// from the SEC1 `EcPrivateKey.publicKey` OPTIONAL field (RFC 5915 §3),
// which all standard tooling (rcgen, OpenSSL, etc.) embeds. If it is
// absent we fail-closed with a documented marker — falling back to
// `Q = d·G` in Rust code would be a FIPS-boundary violation, since that
// scalar multiplication is a cryptographic primitive on the private key
// performed by non-CMVP-validated code. See ADR 0004 §"FIPS posture".
//
// The signing path itself never touches this code — it goes straight to
// corecrypto via `SecKey::create_signature`.
// =========================================================================

fn extract_x963_from_pkcs8(pkcs8_der: &[u8]) -> Result<(Zeroizing<Vec<u8>>, EcCurve), Error> {
    let info = PrivateKeyInfoRef::try_from(pkcs8_der)
        .map_err(|e| Error::General(format!("PKCS#8 parse failed: {e}")))?;
    if info.algorithm.oid != ID_EC_PUBLIC_KEY {
        return Err(Error::General(format!(
            "PKCS#8 algorithm OID is not id-ecPublicKey: got {}",
            info.algorithm.oid
        )));
    }
    // The curve OID lives in algorithm.parameters as an ANY field encoded
    // as an OBJECT IDENTIFIER (named-curve form, RFC 5480 §2.1.1).
    let params = info.algorithm.parameters.ok_or_else(|| {
        Error::General("PKCS#8 EC key missing algorithm.parameters (named curve OID)".to_owned())
    })?;
    let curve_oid: ObjectIdentifier = params
        .decode_as()
        .map_err(|e| Error::General(format!("PKCS#8 EC parameters not an OID: {e}")))?;
    let curve = EcCurve::from_oid(&curve_oid).ok_or_else(|| {
        Error::General(format!(
            "PKCS#8 EC curve OID {curve_oid} is not P-256 / P-384 / P-521"
        ))
    })?;

    // The privateKey OCTET STRING contains the SEC1 ECPrivateKey DER.
    build_x963_from_sec1_with_curve(info.private_key.as_bytes(), curve)
}

fn extract_x963_from_sec1(sec1_der: &[u8]) -> Result<(Zeroizing<Vec<u8>>, EcCurve), Error> {
    // For bare SEC1 input the curve OID is carried in the OPTIONAL
    // `parameters` field of `EcPrivateKey` itself.
    let key = EcPrivateKey::try_from(sec1_der)
        .map_err(|e| Error::General(format!("SEC1 parse failed: {e}")))?;
    let curve_oid = key
        .parameters
        .and_then(|p| p.named_curve())
        .ok_or_else(|| {
            Error::General(
                "SEC1 EC key missing parameters; cannot determine curve without \
                 an outer PKCS#8 wrapper. Re-export with `-pkeyopt ec_param_enc:named_curve` \
                 or wrap in PKCS#8."
                    .to_owned(),
            )
        })?;
    let curve = EcCurve::from_oid(&curve_oid).ok_or_else(|| {
        Error::General(format!(
            "SEC1 EC curve OID {curve_oid} is not P-256 / P-384 / P-521"
        ))
    })?;
    assemble_x963(&key, curve)
}

/// Inner helper: parse the SEC1 ECPrivateKey content (already extracted
/// from a PKCS#8 wrapper) and validate against the curve we determined
/// from the outer envelope's algorithm OID.
fn build_x963_from_sec1_with_curve(
    sec1_octet_string: &[u8],
    curve: EcCurve,
) -> Result<(Zeroizing<Vec<u8>>, EcCurve), Error> {
    let key = EcPrivateKey::try_from(sec1_octet_string)
        .map_err(|e| Error::General(format!("SEC1 inner-parse failed: {e}")))?;
    // If the SEC1 itself ALSO carries a named-curve OID (it's OPTIONAL,
    // but tooling usually duplicates), it must agree with the PKCS#8
    // outer envelope. Disagreement indicates a malformed or
    // adversarially-constructed key.
    if let Some(inner_oid) = key.parameters.and_then(|p| p.named_curve())
        && EcCurve::from_oid(&inner_oid) != Some(curve)
    {
        return Err(Error::General(format!(
            "EC key curve mismatch: PKCS#8 says {curve:?}, SEC1 inner says {inner_oid}"
        )));
    }
    assemble_x963(&key, curve)
}

/// Build Apple's X9.63 blob `0x04 || X || Y || k` from a parsed SEC1
/// `EcPrivateKey`. The blob contains the private scalar, so it is held
/// in a [`Zeroizing`] buffer — heap memory is wiped on drop.
///
/// Fail-closed if `publicKey` is absent (RFC 5915 marks it OPTIONAL, but
/// standard tooling always emits it; missing publicKey would force us
/// to derive Q = d·G outside the FIPS boundary, which we refuse — see
/// gear docs and ADR 0004).
fn assemble_x963(
    key: &EcPrivateKey<'_>,
    curve: EcCurve,
) -> Result<(Zeroizing<Vec<u8>>, EcCurve), Error> {
    let coord = curve.coord_bytes();
    let scalar = key.private_key;
    if scalar.len() != coord {
        return Err(Error::General(format!(
            "SEC1 private scalar length {} != expected {} for {curve:?}",
            scalar.len(),
            coord
        )));
    }
    let pub_point = key.public_key.ok_or_else(|| {
        Error::General(
            "SEC1 EC key is missing the embedded publicKey field. The corecrypto \
             provider does not derive the public point from the private scalar \
             (that would be EC scalar-multiplication outside the FIPS-validated \
             module). Provide a key with the publicKey OPTIONAL field embedded \
             (rcgen / OpenSSL do so by default), or use a Keychain-stored key \
             flow (future enhancement, see ADR 0004 §'Out of scope')."
                .to_owned(),
        )
    })?;
    // Expect uncompressed point: 0x04 || X || Y. Compressed (0x02/0x03)
    // would force a point-decompression step outside corecrypto — same
    // FIPS-boundary concern as Q = d·G. Reject.
    let expected_pub_len = 1 + 2 * coord;
    if pub_point.len() != expected_pub_len {
        return Err(Error::General(format!(
            "SEC1 publicKey length {} != expected {} for uncompressed {curve:?}",
            pub_point.len(),
            expected_pub_len
        )));
    }
    if pub_point[0] != 0x04 {
        return Err(Error::General(format!(
            "SEC1 publicKey is not uncompressed (expected 0x04 prefix, got {:#04x}); \
             compressed-point decompression would happen outside the FIPS boundary",
            pub_point[0]
        )));
    }

    let mut blob: Zeroizing<Vec<u8>> =
        Zeroizing::new(Vec::with_capacity(pub_point.len() + scalar.len()));
    blob.extend_from_slice(pub_point);
    blob.extend_from_slice(scalar);
    Ok((blob, curve))
}

#[cfg(test)]
#[path = "ec_tests.rs"]
mod tests;
