use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::SigningError;

/// A validated signing key reference.
///
/// Format: `[a-z0-9-]+`, max 64 characters, min 1 character.
#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct SigningKeyRef(String);

impl<'de> serde::Deserialize<'de> for SigningKeyRef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        SigningKeyRef::new(s).map_err(serde::de::Error::custom)
    }
}

impl SigningKeyRef {
    /// Creates a new `SigningKeyRef` after validating the format.
    ///
    /// # Errors
    ///
    /// Returns `SigningError::InvalidKeyRef` if the input is empty, exceeds 64
    /// characters, or contains characters outside `[a-z0-9-]`.
    #[must_use = "returns a Result that may contain a validation error"]
    pub fn new(value: impl Into<String>) -> Result<Self, SigningError> {
        let value = value.into();
        if value.is_empty() {
            return Err(SigningError::InvalidKeyRef {
                reason: "must not be empty".into(),
            });
        }
        if value.len() > 64 {
            return Err(SigningError::InvalidKeyRef {
                reason: "exceeds maximum length of 64 characters".into(),
            });
        }
        if !value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(SigningError::InvalidKeyRef {
                reason: "contains invalid characters; only [a-z0-9-] are allowed".into(),
            });
        }
        Ok(Self(value))
    }

    /// Returns the key reference as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for SigningKeyRef {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SigningKeyRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SigningKeyRef").field(&self.0).finish()
    }
}

impl std::fmt::Display for SigningKeyRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Signing algorithm identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SigAlg {
    /// ECDSA using P-256 and SHA-256.
    Es256,
}

/// Result of a signing operation.
#[derive(Debug)]
pub struct SignatureResult {
    /// The raw signature bytes.
    pub signature: Vec<u8>,
    /// The key version used to produce this signature.
    pub key_version: u32,
}

/// A versioned public key entry for a signing key.
#[derive(Debug)]
pub struct PublicKeyVersion {
    /// The key version number.
    pub version: u32,
    /// The signing algorithm for this key version.
    pub alg: SigAlg,
    /// PEM-encoded public key.
    pub public_key_pem: String,
}

/// Request to mint a short-lived capability token.
#[derive(Debug, Clone)]
pub struct MintCapabilityRequest {
    /// Tenant context for the minted token.
    pub context_tenant: Uuid,
    /// Optional project context within the tenant.
    pub context_project_id: Option<Uuid>,
    /// Intended audience for the token.
    pub audience: String,
    /// Optional operation the token authorizes.
    pub operation: Option<String>,
    /// Optional resource type the token is scoped to.
    pub resource_type: Option<String>,
}

/// Claims carried inside a minted capability token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityClaims {
    /// Issuer identifier.
    pub iss: String,
    /// Audience.
    pub aud: String,
    /// Subject (caller user id).
    pub sub: Uuid,
    /// Tenant that owns the subject.
    pub subject_tenant: Uuid,
    /// Optional user type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_type: Option<String>,
    /// Tenant context for the capability.
    pub context_tenant: Uuid,
    /// Optional project context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_project_id: Option<Uuid>,
    /// Scopes granted by this token.
    pub scopes: String,
    /// Unique token identifier.
    pub jti: Uuid,
    /// Issued-at time (Unix seconds).
    pub iat: i64,
    /// Expiry time (Unix seconds).
    pub exp: i64,
    /// Optional actor claim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub act: Option<String>,
    /// Optional operation the token authorizes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    /// Optional resource type the token is scoped to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<String>,
}

/// Request to mint a short-lived data-plane grant token (`grant+jwt`).
///
/// The `grants` gear passes the resolved resource identity and the already-clamped
/// TTL; the caller's identity (`sub`, `subject_tenant`) is taken from the
/// [`SecurityContext`] at mint time. `context_tenant` is the resolved resource's
/// owning tenant (may differ from `subject_tenant` under cross-tenant delegation).
#[derive(Debug, Clone)]
pub struct MintGrantRequest {
    /// The single adapter GTS ID the grant is audience-bound to.
    pub audience: String,
    /// The resolved resource's owning tenant (becomes the `context_tenant` claim).
    pub context_tenant: Uuid,
    /// Optional project attribution (omitted when absent; never an authz input).
    pub project_id: Option<Uuid>,
    /// The authoritative RMS resource UUID.
    pub resource_id: Uuid,
    /// The provider external name used as the adapter path `{name}`.
    pub resource_name: String,
    /// The resolved resource type.
    pub resource_type: String,
    /// The closed set of granted operation ids (each id is itself the RBAC action).
    pub operations: Vec<String>,
    /// The grant lifetime in seconds, already clamped by the gear to the smallest
    /// per-operation `max_ttl`.
    pub ttl_secs: u64,
}

/// Claims carried inside a minted grant token (`grant+jwt`).
///
/// The token carries no `nbf`. `resource_id`, `resource_name`, `resource_type`, and
/// `operations` are enforced offline by the adapter; `project_id` is an attribution
/// hint only and is never an authorization input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrantClaims {
    /// Issuer identifier (`{core}/issuers/grant`).
    pub iss: String,
    /// Audience — exactly one adapter's GTS ID.
    pub aud: String,
    /// Subject (caller user id).
    pub sub: Uuid,
    /// Tenant that owns the subject (the caller's home tenant).
    pub subject_tenant: Uuid,
    /// The resolved resource's owning tenant (the authorization anchor).
    pub context_tenant: Uuid,
    /// Optional project attribution (omitted when absent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<Uuid>,
    /// The authoritative RMS resource UUID the adapter binds provider work to.
    pub resource_id: Uuid,
    /// The provider external name used as the adapter path `{name}`.
    pub resource_name: String,
    /// The resolved resource type.
    pub resource_type: String,
    /// The closed list of granted operation ids.
    pub operations: Vec<String>,
    /// Issued-at time (Unix seconds).
    pub iat: i64,
    /// Expiry time (Unix seconds).
    pub exp: i64,
    /// Unique token identifier.
    pub jti: Uuid,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signing_key_ref_valid_roundtrip() {
        let key = SigningKeyRef::new("cap-token-sign").unwrap();
        assert_eq!(key.as_str(), "cap-token-sign");
    }

    #[test]
    fn signing_key_ref_rejects_empty() {
        assert!(SigningKeyRef::new("").is_err());
    }

    #[test]
    fn signing_key_ref_rejects_space() {
        assert!(SigningKeyRef::new("bad name").is_err());
    }

    #[test]
    fn signing_key_ref_rejects_uppercase() {
        assert!(SigningKeyRef::new("UPPER").is_err());
    }

    #[test]
    fn signing_key_ref_boundary_length() {
        assert!(SigningKeyRef::new("a".repeat(64)).is_ok());
        assert!(SigningKeyRef::new("a".repeat(65)).is_err());
    }

    #[test]
    fn signing_key_ref_deserialize_validates() {
        // valid
        let k: SigningKeyRef = serde_json::from_str("\"valid-key\"").unwrap();
        assert_eq!(k.as_str(), "valid-key");
        // space → rejected
        assert!(serde_json::from_str::<SigningKeyRef>("\"bad name\"").is_err());
        // uppercase → rejected
        assert!(serde_json::from_str::<SigningKeyRef>("\"UPPER\"").is_err());
    }
}
