//! AES-256-GCM through the selected native implementation.
//!
//! The wire shape is the common one: a 12-byte nonce per message, a
//! caller-supplied associated-data string, and `ciphertext || 16-byte tag`.
//! Ciphertexts interoperate with the `RustCrypto` `aes-gcm` crate and with
//! `AESGCM` in other libraries, so existing stored rows keep opening.
//!
//! Sealing draws the nonce inside the module (AWS-LC's randomized-nonce
//! AEAD), as FIPS 140-3 IG C.H requires for approved-mode GCM encryption, and
//! returns it to the caller for storage. Opening takes the stored nonce.
//!
//! Routing: AWS-LC for ordinary builds and Linux FIPS. FIPS builds on macOS
//! and Windows have no AEAD route: `CommonCrypto` takes the IV from outside
//! the module and compares the tag outside it, and the CNG provider keeps its
//! AES-GCM private while the facade forbids its own FFI. There,
//! [`Aes256Gcm::new`] reports [`CryptoError::UnsupportedAlgorithm`].

use aws_lc_rs::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, RandomizedNonceKey, UnboundKey};
use zeroize::Zeroizing;

use super::{CryptoError, backend};

/// AES-256 key length in bytes.
pub const KEY_LEN: usize = 32;
/// GCM nonce length in bytes.
pub const NONCE_LEN: usize = 12;
/// GCM authentication tag length in bytes, appended to the ciphertext.
pub const TAG_LEN: usize = 16;
/// Portable plaintext-size limit, matching the other facade primitives.
pub const MAX_MESSAGE_LEN: usize = u32::MAX as usize;

/// Whether this build's backend policy has an AES-GCM route at all.
const ROUTE_AVAILABLE: bool =
    !(cfg!(feature = "fips") && (cfg!(target_os = "macos") || cfg!(target_os = "windows")));

/// An AES-256-GCM key owned by the facade, redacted in debug output and
/// zeroized on drop. Backend copies made while sealing or opening are outside
/// that guarantee.
pub struct Aes256Gcm {
    key: Zeroizing<[u8; KEY_LEN]>,
}

impl std::fmt::Debug for Aes256Gcm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Aes256Gcm").finish_non_exhaustive()
    }
}

impl Aes256Gcm {
    /// Import a 32-byte key and check backend availability.
    ///
    /// # Errors
    /// Returns `UnsupportedAlgorithm` where the FIPS backend has no AEAD
    /// route, a backend configuration error, or `InvalidKey` for any other
    /// length.
    pub fn new(key: &[u8]) -> Result<Self, CryptoError> {
        // Route absence is a build-time fact; report it before any witness.
        if !ROUTE_AVAILABLE {
            return Err(CryptoError::UnsupportedAlgorithm);
        }
        backend::provider()?;
        if key.len() != KEY_LEN {
            return Err(CryptoError::InvalidKey);
        }
        let mut owned = Zeroizing::new([0; KEY_LEN]);
        owned.copy_from_slice(key);
        Ok(Self { key: owned })
    }

    /// Encrypt `plaintext` bound to `aad` under a nonce the module draws,
    /// returning that nonce and `ciphertext || tag`. Store both.
    ///
    /// Random 96-bit nonces are safe for at most 2^32 messages under one key
    /// (NIST SP 800-38D section 8.3); rotate the key before that.
    ///
    /// # Errors
    /// Returns `MessageTooLong` when `plaintext` or `aad` exceed
    /// [`MAX_MESSAGE_LEN`] bytes, and `SealingFailed` when the backend
    /// refuses to seal.
    pub fn seal(
        &self,
        aad: &[u8],
        plaintext: &[u8],
    ) -> Result<([u8; NONCE_LEN], Vec<u8>), CryptoError> {
        if plaintext.len() > MAX_MESSAGE_LEN || aad.len() > MAX_MESSAGE_LEN {
            return Err(CryptoError::MessageTooLong);
        }
        let key = RandomizedNonceKey::new(&AES_256_GCM, &*self.key)
            .map_err(|_| CryptoError::InvalidKey)?;
        let mut out = Vec::with_capacity(plaintext.len() + TAG_LEN);
        out.extend_from_slice(plaintext);
        let (nonce, tag) = key
            .seal_in_place_separate_tag(Aad::from(aad), &mut out)
            .map_err(|_| CryptoError::SealingFailed)?;
        out.extend_from_slice(tag.as_ref());
        Ok((*nonce.as_ref(), out))
    }

    /// Decrypt `ciphertext || tag` bound to `aad` under the stored `nonce`.
    /// The plaintext is returned in a zeroizing buffer.
    ///
    /// # Errors
    /// Returns `InvalidTag` when the input is shorter than a tag, was
    /// tampered with, or was sealed under another key, nonce or `aad`;
    /// `MessageTooLong` when the plaintext or `aad` would exceed
    /// [`MAX_MESSAGE_LEN`] bytes.
    pub fn open(
        &self,
        nonce: &[u8; NONCE_LEN],
        aad: &[u8],
        ciphertext: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
        let Some(body_len) = ciphertext.len().checked_sub(TAG_LEN) else {
            return Err(CryptoError::InvalidTag);
        };
        if body_len > MAX_MESSAGE_LEN || aad.len() > MAX_MESSAGE_LEN {
            return Err(CryptoError::MessageTooLong);
        }
        // Opening with a caller nonce is an approved service; IG C.H covers
        // encryption only.
        let key = LessSafeKey::new(
            UnboundKey::new(&AES_256_GCM, &*self.key).map_err(|_| CryptoError::InvalidKey)?,
        );
        let mut buffer = Zeroizing::new(ciphertext.to_vec());
        let len = key
            .open_in_place(
                Nonce::assume_unique_for_key(*nonce),
                Aad::from(aad),
                &mut buffer,
            )
            .map_err(|_| CryptoError::InvalidTag)?
            .len();
        buffer.truncate(len);
        Ok(buffer)
    }
}

#[cfg(test)]
#[path = "aead_tests.rs"]
mod tests;
