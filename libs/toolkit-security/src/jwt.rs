//! JWT signing and signature verification through the facade, with
//! jsonwebtoken's existing claim validation. This adapter owns the
//! signature-disabled second pass; callers cannot obtain claims without first
//! verifying the original token. Signing never constructs a jsonwebtoken
//! signer: the compact JWS is assembled here over a facade signature.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::errors::{ErrorKind, Result};
use jsonwebtoken::{
    Algorithm, AlgorithmFamily, DecodingKey, DecodingKeyKind, Header, TokenData, Validation,
};
use pkcs1::der::Decode;
use serde::{Serialize, de::DeserializeOwned};

use crate::crypto::{
    CryptoError,
    signature::{
        EcdsaAlgorithm, EcdsaP256KeyPair, RsaAlgorithm, fixed_to_der,
        verify_ecdsa as verify_ecdsa_der, verify_rsa_components,
    },
};

/// Encode and sign a JWT with `ES256` through the configured OS backend.
///
/// The header and claims are serialized exactly as `jsonwebtoken::encode`
/// does, so `kid`, `typ` and custom claims round-trip through either decoder.
/// Only `ES256` is supported: the facade owns P-256 keys and no other curve.
/// `header.alg` must already say so; the header is signed as given.
///
/// # Errors
/// Returns `InvalidAlgorithm` for any other `alg`, `Json` when the header or
/// claims do not serialize, and `Provider` when the backend cannot sign.
pub fn encode<T: Serialize>(header: &Header, claims: &T, key: &EcdsaP256KeyPair) -> Result<String> {
    if header.alg != Algorithm::ES256 {
        return Err(ErrorKind::InvalidAlgorithm.into());
    }
    let mut token = URL_SAFE_NO_PAD.encode(serde_json::to_vec(header)?);
    token.push('.');
    token.push_str(&URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims)?));
    let signature = key
        .sign_fixed(token.as_bytes())
        .map_err(|error| ErrorKind::Provider(error.to_string()))?;
    token.push('.');
    token.push_str(&URL_SAFE_NO_PAD.encode(signature));
    Ok(token)
}

/// Decode a JWT, verifying its signature with the configured OS backend and
/// validating claims with the caller's original jsonwebtoken policy.
///
/// Signature verification is always required, even if the caller disabled it
/// in `validation`. Existing RS/PS and ES256/384 consumers are supported;
/// other algorithms fail without fallback. EC decoding keys must contain an
/// uncompressed SEC1 public point (`0x04 || X || Y`), as produced by EC JWK
/// components. Compressed points and SPKI-wrapped EC keys are not accepted.
///
/// # Errors
/// Returns jsonwebtoken errors for invalid signatures, keys, algorithms, token
/// encodings or claims. The original token bytes are verified before decoding.
pub fn decode<T: DeserializeOwned>(
    token: &str,
    key: &DecodingKey,
    validation: &Validation,
) -> Result<TokenData<T>> {
    let header = jsonwebtoken::decode_header(token)?;
    if validation.algorithms.is_empty() {
        return Err(ErrorKind::MissingAlgorithm.into());
    }
    if !validation.algorithms.contains(&header.alg)
        || validation
            .algorithms
            .iter()
            .any(|alg| family(*alg) != key.family())
    {
        return Err(ErrorKind::InvalidAlgorithm.into());
    }
    verify(token, key, &header)?;
    let mut claims_policy = validation.clone();
    // verify() above authenticates the original token through the native facade.
    // Disable only the duplicate signature check, retaining claim validation.
    // dangerous::insecure_decode would also skip claims validation.
    #[allow(deprecated)]
    claims_policy.insecure_disable_signature_validation();
    // jsonwebtoken 10.3 still constructs a verifier here, but its AWS-LC
    // constructors only check the family and clone the key. No primitive is
    // called: signature decoding and verify() are behind validate_signature.
    jsonwebtoken::decode(token, key, &claims_policy)
}

// Private: header must come from decode_header(token) in the verified adapter.
fn verify(token: &str, key: &DecodingKey, header: &Header) -> Result<()> {
    let parts: Vec<_> = token.splitn(4, '.').collect();
    if parts.len() != 3 {
        return Err(ErrorKind::InvalidToken.into());
    }
    if family(header.alg) != key.family() {
        return Err(ErrorKind::InvalidAlgorithm.into());
    }
    let message = &token.as_bytes()[..parts[0].len() + 1 + parts[1].len()];
    let signature = URL_SAFE_NO_PAD
        .decode(parts[2])
        .map_err(|_| jsonwebtoken::errors::Error::from(ErrorKind::InvalidSignature))?;
    match header.alg {
        Algorithm::RS256
        | Algorithm::RS384
        | Algorithm::RS512
        | Algorithm::PS256
        | Algorithm::PS384
        | Algorithm::PS512 => {
            let algorithm = match header.alg {
                Algorithm::RS256 => RsaAlgorithm::Pkcs1Sha256,
                Algorithm::RS384 => RsaAlgorithm::Pkcs1Sha384,
                Algorithm::RS512 => RsaAlgorithm::Pkcs1Sha512,
                Algorithm::PS256 => RsaAlgorithm::PssSha256,
                Algorithm::PS384 => RsaAlgorithm::PssSha384,
                Algorithm::PS512 => RsaAlgorithm::PssSha512,
                _ => unreachable!("RSA branch"),
            };
            match key.kind() {
                DecodingKeyKind::RsaModulusExponent { n, e } => {
                    verify_rsa_components(algorithm, n, e, message, &signature)
                        .map_err(map_signature_error)
                }
                DecodingKeyKind::SecretOrDer(der) => {
                    let key = rsa_public_key(der).map_err(map_signature_error)?;
                    verify_rsa_components(
                        algorithm,
                        key.modulus.as_bytes(),
                        key.public_exponent.as_bytes(),
                        message,
                        &signature,
                    )
                    .map_err(map_signature_error)
                }
            }
        }
        Algorithm::ES256 | Algorithm::ES384 => {
            verify_ecdsa(header.alg, key.as_bytes(), message, &signature)
                .map_err(map_signature_error)
        }
        // HMAC verifier constructors initialize AWS-LC state even with
        // verification disabled. This adapter supports only the existing
        // RSA/ECDSA consumers whose constructors perform no second-pass crypto.
        Algorithm::HS256 | Algorithm::HS384 | Algorithm::HS512 | Algorithm::EdDSA => {
            Err(ErrorKind::InvalidAlgorithm.into())
        }
    }
}

fn map_signature_error(error: CryptoError) -> jsonwebtoken::errors::Error {
    match error {
        CryptoError::InvalidKey | CryptoError::InvalidSignature | CryptoError::MessageTooLong => {
            ErrorKind::InvalidSignature.into()
        }
        _ => ErrorKind::Provider(error.to_string()).into(),
    }
}

fn rsa_public_key(bytes: &[u8]) -> std::result::Result<pkcs1::RsaPublicKey<'_>, CryptoError> {
    if let Ok(key) = pkcs1::RsaPublicKey::from_der(bytes) {
        return Ok(key);
    }
    let spki =
        spki::SubjectPublicKeyInfoRef::from_der(bytes).map_err(|_| CryptoError::InvalidKey)?;
    // Only rsaEncryption: never discard RSA-PSS algorithm/key constraints.
    if spki.algorithm.oid != pkcs1::ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.1")
        || spki
            .algorithm
            .parameters
            .is_some_and(|params| params != pkcs1::der::asn1::AnyRef::NULL)
    {
        return Err(CryptoError::InvalidKey);
    }
    let bytes = spki
        .subject_public_key
        .as_bytes()
        .ok_or(CryptoError::InvalidKey)?;
    pkcs1::RsaPublicKey::from_der(bytes).map_err(|_| CryptoError::InvalidKey)
}

fn verify_ecdsa(
    algorithm: Algorithm,
    key: &[u8],
    message: &[u8],
    signature: &[u8],
) -> std::result::Result<(), CryptoError> {
    let (size, primitive) = match algorithm {
        Algorithm::ES256 => (32, EcdsaAlgorithm::P256Sha256),
        Algorithm::ES384 => (48, EcdsaAlgorithm::P384Sha384),
        _ => return Err(CryptoError::UnsupportedAlgorithm),
    };
    if signature.len() != size * 2 || key.len() != size * 2 + 1 || key[0] != 4 {
        return Err(CryptoError::InvalidSignature);
    }
    if message.len() > u32::MAX as usize {
        return Err(CryptoError::MessageTooLong);
    }
    // JOSE uses fixed-width r||s; the TLS verifier expects ASN.1 DER.
    verify_ecdsa_der(primitive, key, message, &fixed_to_der(signature, size)?)
}

fn family(algorithm: Algorithm) -> AlgorithmFamily {
    match algorithm {
        Algorithm::RS256
        | Algorithm::RS384
        | Algorithm::RS512
        | Algorithm::PS256
        | Algorithm::PS384
        | Algorithm::PS512 => AlgorithmFamily::Rsa,
        Algorithm::ES256 | Algorithm::ES384 => AlgorithmFamily::Ec,
        Algorithm::HS256 | Algorithm::HS384 | Algorithm::HS512 => AlgorithmFamily::Hmac,
        Algorithm::EdDSA => AlgorithmFamily::Ed,
    }
}

#[cfg(test)]
#[path = "jwt_tests.rs"]
mod tests;
