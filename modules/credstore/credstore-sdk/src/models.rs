// Updated: 2026-03-18 by Constructor Tech
use std::fmt;

use serde::de::Deserializer;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::Zeroize;

use crate::error::CredStoreError;

/// Re-export from tenant-resolver-sdk for cross-module type consistency.
pub use tenant_resolver_sdk::TenantId;

/// Owner identifier, representing `SecurityContext.subject_id()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OwnerId(pub Uuid);

impl OwnerId {
    /// Returns the nil UUID wrapped as an `OwnerId`.
    #[must_use]
    pub fn nil() -> Self {
        Self(Uuid::nil())
    }

    /// Returns `true` if the inner UUID is the nil UUID.
    #[must_use]
    pub fn is_nil(&self) -> bool {
        self.0.is_nil()
    }
}

impl fmt::Display for OwnerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// A validated secret reference key.
///
/// Format: `[a-zA-Z0-9_-]+`, max 255 characters.
/// Colons are prohibited to prevent `ExternalID` collisions in backend storage.
#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
pub struct SecretRef(String);

impl<'de> Deserialize<'de> for SecretRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        SecretRef::new(s).map_err(serde::de::Error::custom)
    }
}

impl SecretRef {
    /// Creates a new `SecretRef` after validating the format.
    ///
    /// # Errors
    ///
    /// Returns `CredStoreError::InvalidSecretRef` if the input is empty,
    /// exceeds 255 characters, or contains characters outside `[a-zA-Z0-9_-]`.
    #[must_use = "returns a Result that may contain a validation error"]
    pub fn new(value: impl Into<String>) -> Result<Self, CredStoreError> {
        let value = value.into();
        if value.is_empty() {
            return Err(CredStoreError::invalid_ref("must not be empty"));
        }
        if value.len() > 255 {
            return Err(CredStoreError::invalid_ref(
                "exceeds maximum length of 255 characters",
            ));
        }
        if !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(CredStoreError::invalid_ref(
                "contains invalid characters; only [a-zA-Z0-9_-] are allowed",
            ));
        }
        Ok(Self(value))
    }
}

impl AsRef<str> for SecretRef {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SecretRef").field(&self.0).finish()
    }
}

/// A secret value with redacted Debug/Display output.
///
/// Wraps opaque bytes (`Vec<u8>`) and guarantees that content is never
/// leaked through formatting. Does not implement `Serialize`/`Deserialize`
/// to prevent accidental serialization of secret data.
pub struct SecretValue(Vec<u8>);

impl SecretValue {
    /// Creates a new `SecretValue` from raw bytes.
    #[must_use]
    pub fn new(value: Vec<u8>) -> Self {
        Self(value)
    }

    /// Returns a reference to the raw bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl From<Vec<u8>> for SecretValue {
    fn from(value: Vec<u8>) -> Self {
        Self(value)
    }
}

impl From<String> for SecretValue {
    fn from(value: String) -> Self {
        Self(value.into_bytes())
    }
}

impl From<&str> for SecretValue {
    fn from(value: &str) -> Self {
        Self(value.as_bytes().to_vec())
    }
}

impl Drop for SecretValue {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

impl fmt::Display for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

/// Controls the visibility scope of a stored secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharingMode {
    /// Only the owner can access the secret.
    Private,
    /// All users within the owner's tenant can access the secret.
    #[default]
    Tenant,
    /// The secret is accessible across tenant boundaries.
    Shared,
}

/// Response returned by [`CredStoreClientV1::get`](crate::CredStoreClientV1::get)
/// containing the secret value and access metadata.
#[derive(Debug)]
pub struct GetSecretResponse {
    /// The decrypted secret value.
    pub value: SecretValue,
    /// The tenant that owns this secret (may differ from the requesting tenant
    /// when the secret is inherited via hierarchical resolution).
    pub owner_tenant_id: TenantId,
    /// The sharing mode of the secret.
    pub sharing: SharingMode,
    /// `true` if the secret was retrieved from an ancestor tenant via
    /// hierarchical resolution, `false` if owned by the requesting tenant.
    pub is_inherited: bool,
}

/// Metadata returned by plugins alongside the secret value.
#[derive(Debug)]
pub struct SecretMetadata {
    pub value: SecretValue,
    pub owner_id: OwnerId,
    pub sharing: SharingMode,
    pub owner_tenant_id: TenantId,
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;
