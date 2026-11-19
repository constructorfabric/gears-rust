//! AWS Signature Version 4 primitives: the credential scope, the signing-key
//! HMAC chain, the final signature and lowercase-hex SHA-256.
//!
//! Building the canonical request and the string to sign is HTTP protocol
//! logic and stays with the caller. Every HMAC and hash runs through
//! [`HmacSha256`] and [`Sha256`], so FIPS builds use the selected native
//! provider.

use zeroize::Zeroizing;

use super::hash::Sha256;
use super::mac::HmacSha256;
use super::{CryptoError, backend};

const SECRET_PREFIX: &[u8] = b"AWS4";
const SCOPE_TERMINATOR: &str = "aws4_request";
const DATE_LEN: usize = 8;
const MAX_SCOPE_PART_LEN: usize = 64;

/// A validated credential scope, `date/region/service/aws4_request`.
///
/// The same value goes into [`SigningKey::derive`], the string to sign and
/// the `Credential=` part of the `Authorization` header.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CredentialScope {
    scope: String,
    region_len: usize,
    service_len: usize,
}

impl CredentialScope {
    /// Build the scope for `date` (`YYYYMMDD`, the date of the request
    /// timestamp, not the timestamp itself), `region` and `service` (the
    /// signing name, e.g. `bedrock`).
    ///
    /// # Errors
    /// Returns `InvalidCredentialScope` for a date that is not `YYYYMMDD` with
    /// month 01-12 and day 01-31, or a region or service that is empty, longer
    /// than 64 bytes or not made of ASCII letters, digits, `-`, `_` and `.`.
    /// AWS regions and signing names are lowercase; S3-compatible servers may
    /// use others, such as `RegionOne`.
    pub fn new(date: &str, region: &str, service: &str) -> Result<Self, CryptoError> {
        if !is_scope_date(date) || !is_scope_part(region) || !is_scope_part(service) {
            return Err(CryptoError::InvalidCredentialScope);
        }
        Ok(Self {
            scope: format!("{date}/{region}/{service}/{SCOPE_TERMINATOR}"),
            region_len: region.len(),
            service_len: service.len(),
        })
    }

    /// The scope date, `YYYYMMDD`.
    #[must_use]
    pub fn date(&self) -> &str {
        &self.scope[..DATE_LEN]
    }

    /// The region.
    #[must_use]
    pub fn region(&self) -> &str {
        let start = DATE_LEN + 1;
        &self.scope[start..start + self.region_len]
    }

    /// The service signing name.
    #[must_use]
    pub fn service(&self) -> &str {
        let start = DATE_LEN + 1 + self.region_len + 1;
        &self.scope[start..start + self.service_len]
    }

    /// The whole scope, `date/region/service/aws4_request`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.scope
    }
}

impl std::fmt::Display for CredentialScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.scope)
    }
}

fn is_scope_date(date: &str) -> bool {
    let bytes = date.as_bytes();
    if bytes.len() != DATE_LEN || !bytes.iter().all(u8::is_ascii_digit) {
        return false;
    }
    let two = |i: usize| (bytes[i] - b'0') * 10 + (bytes[i + 1] - b'0');
    (1..=12).contains(&two(4)) && (1..=31).contains(&two(6))
}

fn is_scope_part(part: &str) -> bool {
    !part.is_empty()
        && part.len() <= MAX_SCOPE_PART_LEN
        && part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// A derived Signature Version 4 signing key (`kSigning`) for one credential
/// scope.
///
/// The key is redacted in debug output. The final and intermediate keys held
/// by the facade are zeroized on drop; copies made by rustls or OS crypto
/// APIs are outside that guarantee.
pub struct SigningKey {
    mac: HmacSha256,
    scope: CredentialScope,
}

impl std::fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SigningKey")
            .field("scope", &self.scope.as_str())
            .finish_non_exhaustive()
    }
}

impl SigningKey {
    /// Derive `kSigning` from a secret access key and a credential scope.
    ///
    /// # Errors
    /// Returns `InvalidKey` for an empty secret. FIPS builds return
    /// `HmacKeyTooShort` for a secret shorter than 10 bytes, because the first
    /// HMAC key, `"AWS4" + secret`, must hold at least 14 bytes. Returns a
    /// configuration error if the selected backend is unavailable.
    pub fn derive(secret_access_key: &[u8], scope: &CredentialScope) -> Result<Self, CryptoError> {
        let key = derive_key(secret_access_key, scope)?;
        Ok(Self {
            mac: HmacSha256::new(key.as_ref())?,
            scope: scope.clone(),
        })
    }

    /// The scope the key was derived for. A caller that keeps keys across
    /// requests compares its date with the request's before reusing one.
    #[must_use]
    pub fn scope(&self) -> &CredentialScope {
        &self.scope
    }

    /// Sign a string to sign, returning the lowercase-hex `Signature` value.
    ///
    /// # Errors
    /// Returns `MessageTooLong` above [`super::mac::MAX_MESSAGE_LEN`] bytes.
    pub fn signature(&self, string_to_sign: &[u8]) -> Result<String, CryptoError> {
        self.mac.sign(string_to_sign).map(|tag| hex_lower(&tag))
    }
}

/// Lowercase-hex SHA-256, as Signature Version 4 uses for the payload hash and the
/// canonical-request hash.
///
/// # Errors
/// Returns a configuration error if the selected backend is unavailable.
pub fn sha256_hex(bytes: &[u8]) -> Result<String, CryptoError> {
    let mut hash = Sha256::with_suite(backend::sha256_suite()?);
    hash.update(bytes);
    Ok(hex_lower(&hash.finalize()))
}

fn derive_key(
    secret_access_key: &[u8],
    scope: &CredentialScope,
) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
    if secret_access_key.is_empty() {
        return Err(CryptoError::InvalidKey);
    }
    // Sized up front so the secret is never left behind by a reallocation.
    let mut k_secret = Zeroizing::new(Vec::with_capacity(
        SECRET_PREFIX.len() + secret_access_key.len(),
    ));
    k_secret.extend_from_slice(SECRET_PREFIX);
    k_secret.extend_from_slice(secret_access_key);

    let k_date = hmac(&k_secret, scope.date().as_bytes())?;
    let k_region = hmac(k_date.as_ref(), scope.region().as_bytes())?;
    let k_service = hmac(k_region.as_ref(), scope.service().as_bytes())?;
    hmac(k_service.as_ref(), SCOPE_TERMINATOR.as_bytes())
}

fn hmac(key: &[u8], message: &[u8]) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
    HmacSha256::new(key)?.sign(message).map(Zeroizing::new)
}

fn hex_lower(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

#[cfg(test)]
#[path = "sigv4_tests.rs"]
mod tests;
