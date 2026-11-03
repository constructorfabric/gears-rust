//! ECDSA signing and verification. All curve operations stay inside the selected backend.

use std::sync::Arc;

use pkcs8::der::Decode;
#[cfg(all(feature = "fips", any(target_os = "macos", target_os = "windows")))]
use pkcs8::der::Encode;
use pkcs8::{ObjectIdentifier, PrivateKeyInfoRef};
use rustls::SignatureScheme;
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer, alg_id};
use rustls::sign::SigningKey;
use zeroize::Zeroizing;

use super::{CryptoError, backend};

const EC_PUBLIC_KEY: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");
const P256: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.3.1.7");
const SCHEME: SignatureScheme = SignatureScheme::ECDSA_NISTP256_SHA256;
const PAIRWISE_MESSAGE: &[u8] = b"toolkit-security ECDSA P-256 key consistency";

/// Fixed curve/hash pairs supported by native ECDSA verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EcdsaAlgorithm {
    /// NIST P-256 with SHA-256.
    P256Sha256,
    /// NIST P-384 with SHA-384.
    P384Sha384,
}

/// An ECDSA P-256 signing key owned by the configured OS backend.
///
/// Private keys use PKCS#8 DER with an embedded uncompressed SEC1 public point.
/// Public keys use `0x04 || X || Y` (65 bytes); signatures use ASN.1 DER, not
/// fixed-width JOSE `r || s`. No backend key types escape this API.
pub struct EcdsaP256KeyPair {
    inner: Arc<dyn SigningKey>,
    public_key: Vec<u8>,
}

/// An uncompressed P-256 public key for verifying ASN.1 DER signatures.
#[derive(Clone)]
pub struct EcdsaP256VerifyingKey(Vec<u8>);

impl EcdsaP256VerifyingKey {
    /// Read an uncompressed SEC1 point and check backend availability.
    ///
    /// # Errors
    /// Rejects unavailable backends and incorrect key encodings. The backend
    /// checks the curve point when verifying a signature.
    pub fn from_sec1_bytes(bytes: &[u8]) -> Result<Self, CryptoError> {
        validate_public_key(bytes)?;
        Ok(Self(bytes.to_vec()))
    }

    /// Return the uncompressed SEC1 point.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Verify a P-256/SHA-256 ASN.1 DER signature.
    ///
    /// # Errors
    /// Returns an error if the signature or curve point is invalid.
    pub fn verify(&self, message: &[u8], signature: &[u8]) -> Result<(), CryptoError> {
        verify_ecdsa_p256(&self.0, message, signature)
    }
}

impl EcdsaP256KeyPair {
    /// Generate a fresh key inside the selected backend.
    ///
    /// # Errors
    /// Returns an error if the backend is unavailable or key generation fails.
    pub fn generate() -> Result<Self, CryptoError> {
        Self::from_pkcs8(&Self::generate_pkcs8()?)
    }

    /// Generate an exportable PKCS#8 DER key using the selected backend.
    /// The returned private bytes are erased when dropped.
    ///
    /// # Errors
    /// Returns an error if backend validation or native key generation fails.
    pub fn generate_pkcs8() -> Result<Zeroizing<Vec<u8>>, CryptoError> {
        backend::provider()?;
        generate_pkcs8()
    }

    /// Import PKCS#8 DER, requiring id-ecPublicKey, named curve P-256 and an
    /// embedded uncompressed public point. Checks the key pair using the backend.
    ///
    /// # Errors
    /// Returns an error for unsupported encodings, inconsistent keys or an
    /// unavailable backend. Ed25519 seeds are never interpreted as EC scalars.
    pub fn from_pkcs8(bytes: &[u8]) -> Result<Self, CryptoError> {
        Self::from_pkcs8_with_provider(bytes, backend::provider()?)
    }

    pub(super) fn from_pkcs8_with_provider(
        bytes: &[u8],
        provider: &rustls::crypto::CryptoProvider,
    ) -> Result<Self, CryptoError> {
        let pki = PrivateKeyInfoRef::from_der(bytes).map_err(|_| CryptoError::InvalidKey)?;
        if pki.algorithm.oid != EC_PUBLIC_KEY || pki.algorithm.parameters_oid().ok() != Some(P256) {
            return Err(CryptoError::InvalidKey);
        }
        let ec = sec1::EcPrivateKey::from_der(pki.private_key.as_bytes())
            .map_err(|_| CryptoError::InvalidKey)?;
        if ec.private_key.len() != 32
            || ec
                .parameters
                .is_some_and(|params| params.named_curve() != Some(P256))
        {
            return Err(CryptoError::InvalidKey);
        }
        let public_key = ec.public_key.ok_or(CryptoError::InvalidKey)?;
        validate_public_key_encoding(public_key)?;
        let inner = provider
            .key_provider
            .load_private_key(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
                bytes.to_vec(),
            )))
            .map_err(|_| CryptoError::InvalidKey)?;
        let key = Self {
            inner,
            public_key: public_key.to_vec(),
        };
        let signature = key.sign(PAIRWISE_MESSAGE)?;
        verify_with_provider(provider, key.public_key(), PAIRWISE_MESSAGE, &signature)
            .map_err(|_| CryptoError::InvalidKey)?;
        Ok(key)
    }

    /// Return the uncompressed SEC1 public point (65 bytes).
    #[must_use]
    pub fn public_key(&self) -> &[u8] {
        &self.public_key
    }

    /// Return the public half of this validated key pair.
    #[must_use]
    pub fn verifying_key(&self) -> EcdsaP256VerifyingKey {
        EcdsaP256VerifyingKey(self.public_key.clone())
    }

    /// Sign the message with P-256/SHA-256, returning an ASN.1 DER signature.
    ///
    /// # Errors
    /// Returns an error for an oversized message or native signing failure.
    pub fn sign(&self, message: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if message.len() > u32::MAX as usize {
            return Err(CryptoError::MessageTooLong);
        }
        self.inner
            .choose_scheme(&[SCHEME])
            .ok_or(CryptoError::UnsupportedAlgorithm)?
            .sign(message)
            .map_err(|_| CryptoError::SigningFailed)
    }
}

pub(super) fn validate_public_key(key: &[u8]) -> Result<(), CryptoError> {
    backend::provider()?;
    validate_public_key_encoding(key)
}

fn validate_public_key_encoding(key: &[u8]) -> Result<(), CryptoError> {
    if key.len() != 65 || key.first() != Some(&4) {
        return Err(CryptoError::InvalidKey);
    }
    Ok(())
}

/// Verify a P-256/SHA-256 ASN.1 DER signature with an uncompressed SEC1 key.
///
/// # Errors
/// Returns an error for malformed inputs, a mismatched signature or an
/// unavailable backend. Other curves and hash algorithms are never tried.
pub fn verify_ecdsa_p256(key: &[u8], message: &[u8], signature: &[u8]) -> Result<(), CryptoError> {
    verify_with_provider(backend::provider()?, key, message, signature)
}

pub(super) fn verify_with_provider(
    provider: &rustls::crypto::CryptoProvider,
    key: &[u8],
    message: &[u8],
    signature: &[u8],
) -> Result<(), CryptoError> {
    verify_signature_with_provider(
        provider,
        EcdsaAlgorithm::P256Sha256,
        key,
        message,
        signature,
    )
}

/// Verify an ASN.1 DER ECDSA signature with an uncompressed SEC1 public key.
///
/// # Errors
/// Returns an error for malformed inputs, a signature mismatch or an unavailable
/// backend. The curve/hash pair is exact; no alternative algorithm is tried.
pub fn verify_ecdsa(
    algorithm: EcdsaAlgorithm,
    key: &[u8],
    message: &[u8],
    signature: &[u8],
) -> Result<(), CryptoError> {
    verify_signature_with_provider(backend::provider()?, algorithm, key, message, signature)
}

pub(super) fn verify_signature_with_provider(
    provider: &rustls::crypto::CryptoProvider,
    algorithm: EcdsaAlgorithm,
    key: &[u8],
    message: &[u8],
    signature: &[u8],
) -> Result<(), CryptoError> {
    let (key_len, scheme, key_id, signature_id) = match algorithm {
        EcdsaAlgorithm::P256Sha256 => (65, SCHEME, alg_id::ECDSA_P256, alg_id::ECDSA_SHA256),
        EcdsaAlgorithm::P384Sha384 => (
            97,
            SignatureScheme::ECDSA_NISTP384_SHA384,
            alg_id::ECDSA_P384,
            alg_id::ECDSA_SHA384,
        ),
    };
    if key.len() != key_len || key.first() != Some(&4) {
        return Err(CryptoError::InvalidKey);
    }
    if message.len() > u32::MAX as usize {
        return Err(CryptoError::MessageTooLong);
    }
    let algorithms = provider
        .signature_verification_algorithms
        .mapping
        .iter()
        .find(|(candidate, _)| *candidate == scheme)
        .ok_or(CryptoError::UnsupportedAlgorithm)?
        .1;
    if algorithms.iter().any(|algorithm| {
        algorithm.public_key_alg_id() == key_id
            && algorithm.signature_alg_id() == signature_id
            && algorithm.verify_signature(key, message, signature).is_ok()
    }) {
        Ok(())
    } else {
        Err(CryptoError::InvalidSignature)
    }
}

// Structural DER encoding only; no curve arithmetic outside the native module.
#[cfg(all(feature = "fips", any(target_os = "macos", target_os = "windows")))]
fn encode_pkcs8(private: &[u8], public: &[u8]) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    let ec = sec1::EcPrivateKey {
        private_key: private,
        parameters: None,
        public_key: Some(public),
    };
    let ec_der = Zeroizing::new(ec.to_der().map_err(|_| CryptoError::InvalidKey)?);
    let algorithm = pkcs8::AlgorithmIdentifierRef {
        oid: EC_PUBLIC_KEY,
        parameters: Some((&P256).into()),
    };
    let pki = PrivateKeyInfoRef::new(
        algorithm,
        pkcs8::der::asn1::OctetStringRef::new(&ec_der).map_err(|_| CryptoError::InvalidKey)?,
    );
    Ok(Zeroizing::new(
        pki.to_der().map_err(|_| CryptoError::InvalidKey)?,
    ))
}

#[cfg(all(feature = "fips", target_os = "macos"))]
pub(super) fn generate_pkcs8() -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    use security_framework::key::{GenerateKeyOptions, KeyType, SecKey};
    let mut options = GenerateKeyOptions::default();
    options.set_key_type(KeyType::ec()).set_size_in_bits(256);
    let key = SecKey::new(&options).map_err(|_| CryptoError::KeyGenerationFailed)?;
    let data = key
        .external_representation()
        .ok_or(CryptoError::KeyGenerationFailed)?;
    // Apple's X9.63 private encoding is 0x04 || X || Y || d.
    let bytes = Zeroizing::new(data.to_vec());
    if bytes.len() != 97 || bytes[0] != 4 {
        return Err(CryptoError::InvalidKey);
    }
    encode_pkcs8(&bytes[65..], &bytes[..65])
}

#[cfg(all(feature = "fips", target_os = "windows"))]
pub(super) fn generate_pkcs8() -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    use win_crypto_ng::asymmetric::{AsymmetricKey, Ecdsa, Export, ecc::NistP256};
    let key = AsymmetricKey::builder(Ecdsa(NistP256))
        .build()
        .map_err(|_| CryptoError::KeyGenerationFailed)?;
    let blob = key.export().map_err(|_| CryptoError::KeyGenerationFailed)?;
    // Move the entire exported CNG blob into an erasing owner, including d.
    let bytes = Zeroizing::new(blob.into_bytes());
    if bytes.len() != 104 {
        return Err(CryptoError::InvalidKey);
    }
    let mut public = vec![4];
    public.extend_from_slice(&bytes[8..72]);
    encode_pkcs8(&bytes[72..], &public)
}

#[cfg(not(all(feature = "fips", any(target_os = "macos", target_os = "windows"))))]
pub(super) fn generate_pkcs8() -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    use aws_lc_rs::{
        rand::SystemRandom,
        signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair},
    };
    EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &SystemRandom::new())
        .map(|document| Zeroizing::new(document.as_ref().to_vec()))
        .map_err(|_| CryptoError::KeyGenerationFailed)
}

#[cfg(test)]
#[path = "ecdsa_tests.rs"]
mod tests;
