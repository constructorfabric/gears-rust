use std::fmt;

use serde::de::Deserializer;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::CredStoreError;

/// Tenant identifier, matching `tenant-resolver-sdk` convention.
pub type TenantId = Uuid;

/// Owner identifier, representing `SecurityContext.subject_id()`.
pub type OwnerId = Uuid;

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
        self.0.iter_mut().for_each(|b| *b = 0);
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
mod tests {
    use super::*;

    #[test]
    fn secret_ref_valid() {
        assert!(SecretRef::new("partner-openai-key").is_ok());
        assert!(SecretRef::new("api_key_v2").is_ok());
        assert!(SecretRef::new("ABC123").is_ok());
    }

    #[test]
    fn secret_ref_invalid_chars() {
        assert!(SecretRef::new("my:key").is_err());
        assert!(SecretRef::new("my key").is_err());
        assert!(SecretRef::new("key/path").is_err());
    }

    #[test]
    fn secret_ref_empty() {
        assert!(SecretRef::new("").is_err());
    }

    #[test]
    fn secret_ref_too_long() {
        let long = "a".repeat(256);
        assert!(SecretRef::new(long).is_err());
    }

    #[test]
    fn secret_ref_max_length() {
        let max = "a".repeat(255);
        assert!(SecretRef::new(max).is_ok());
    }

    #[test]
    fn secret_ref_deserialize_validates() {
        let valid: Result<SecretRef, _> = serde_json::from_str("\"valid-key_1\"");
        assert!(valid.is_ok());
        assert_eq!(valid.unwrap().as_ref(), "valid-key_1");

        let with_colon: Result<SecretRef, _> = serde_json::from_str("\"my:evil/key\"");
        assert!(with_colon.is_err());

        let empty: Result<SecretRef, _> = serde_json::from_str("\"\"");
        assert!(empty.is_err());
    }

    #[test]
    fn secret_value_debug_redacted() {
        let val = SecretValue::new(b"super-secret".to_vec());
        assert_eq!(format!("{val:?}"), "[REDACTED]");
    }

    #[test]
    fn secret_value_display_redacted() {
        let val = SecretValue::new(b"super-secret".to_vec());
        assert_eq!(format!("{val}"), "[REDACTED]");
    }

    #[test]
    fn secret_value_as_bytes() {
        let val = SecretValue::from("hello");
        assert_eq!(val.as_bytes(), b"hello");
    }

    #[test]
    fn sharing_mode_default_is_tenant() {
        assert_eq!(SharingMode::default(), SharingMode::Tenant);
    }

    #[test]
    fn get_secret_response_debug_redacts_value() {
        let resp = GetSecretResponse {
            value: SecretValue::from("secret"),
            owner_tenant_id: Uuid::nil(),
            sharing: SharingMode::Shared,
            is_inherited: true,
        };
        let debug = format!("{resp:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("secret"));
        assert!(debug.contains("is_inherited: true"));
    }

    #[test]
    fn secret_metadata_debug_redacts_value() {
        let meta = SecretMetadata {
            value: SecretValue::from("secret"),
            owner_id: Uuid::nil(),
            sharing: SharingMode::Tenant,
            owner_tenant_id: Uuid::nil(),
        };
        let debug = format!("{meta:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("secret"));
    }

    #[test]
    fn secret_ref_as_ref_returns_inner_string() {
        let r = SecretRef::new("my-key").unwrap();
        assert_eq!(r.as_ref(), "my-key");
    }

    #[test]
    fn secret_value_new_stores_bytes() {
        let val = SecretValue::new(b"hello world".to_vec());
        assert_eq!(val.as_bytes(), b"hello world");
    }

    #[test]
    fn sharing_mode_serde_roundtrip() {
        for (mode, expected_json) in [
            (SharingMode::Private, "\"private\""),
            (SharingMode::Tenant, "\"tenant\""),
            (SharingMode::Shared, "\"shared\""),
        ] {
            let json = serde_json::to_string(&mode).unwrap();
            assert_eq!(json, expected_json);
            let back: SharingMode = serde_json::from_str(&json).unwrap();
            assert_eq!(back, mode);
        }
    }

    #[test]
    fn sharing_mode_clone_and_copy() {
        let original = SharingMode::Private;
        let copied = original; // Copy
        let cloned = original; // Clone via Copy
        assert_eq!(original, copied);
        assert_eq!(original, cloned);
    }

    #[test]
    fn get_secret_response_is_inherited_false() {
        let resp = GetSecretResponse {
            value: SecretValue::from("s"),
            owner_tenant_id: Uuid::nil(),
            sharing: SharingMode::Tenant,
            is_inherited: false,
        };
        assert!(!resp.is_inherited);
        assert_eq!(resp.sharing, SharingMode::Tenant);
    }

    #[test]
    fn secret_value_from_string_stores_bytes() {
        let val = SecretValue::from(String::from("owned"));
        assert_eq!(val.as_bytes(), b"owned");
    }

    #[test]
    fn secret_value_from_vec_stores_bytes() {
        let val = SecretValue::from(vec![0xDE, 0xAD]);
        assert_eq!(val.as_bytes(), &[0xDE, 0xAD]);
    }

    #[test]
    fn secret_ref_debug_shows_inner_value() {
        let r = SecretRef::new("my-key").unwrap();
        let dbg = format!("{r:?}");
        assert!(dbg.contains("SecretRef"));
        assert!(dbg.contains("my-key"));
    }

    #[test]
    fn secret_ref_serialize_roundtrip() {
        let r = SecretRef::new("round-trip").unwrap();
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(json, "\"round-trip\"");
        let back: SecretRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back.as_ref(), "round-trip");
    }
}
