//! Shared boundary types. No HTTP, database or commercial-provider internals.
use crate::catalog::OrderState;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Invalid typed boundary input.
#[derive(Debug, thiserror::Error)]
#[error("invalid Orders boundary value")]
pub struct InvalidBoundaryValue;

/// Positive SQL integer; sparse commercial version, not execution generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "i64", into = "i64")]
pub struct OrderVersion(i32);
impl TryFrom<i64> for OrderVersion {
    type Error = InvalidBoundaryValue;
    fn try_from(value: i64) -> Result<Self, Self::Error> {
        let value = i32::try_from(value).map_err(|_| InvalidBoundaryValue)?;
        if value <= 0 {
            return Err(InvalidBoundaryValue);
        }
        Ok(Self(value))
    }
}
impl From<OrderVersion> for i64 {
    fn from(value: OrderVersion) -> Self {
        Self::from(value.0)
    }
}

/// Nonnegative signed bigint; zero is the initial working-draft revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "i64", into = "i64")]
pub struct DraftRevision(i64);
impl TryFrom<i64> for DraftRevision {
    type Error = InvalidBoundaryValue;
    fn try_from(value: i64) -> Result<Self, Self::Error> {
        if value < 0 {
            return Err(InvalidBoundaryValue);
        }
        Ok(Self(value))
    }
}
impl From<DraftRevision> for i64 {
    fn from(value: DraftRevision) -> Self {
        value.0
    }
}

/// Exact, unnormalized idempotency bytes. Debug output never logs the key.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct IdempotencyKey(String);
impl std::fmt::Debug for IdempotencyKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("IdempotencyKey([redacted])")
    }
}
impl TryFrom<String> for IdempotencyKey {
    type Error = InvalidBoundaryValue;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() || value.len() > 255 || !value.bytes().all(|b| (32..=126).contains(&b))
        {
            return Err(InvalidBoundaryValue);
        }
        Ok(Self(value))
    }
}
impl From<IdempotencyKey> for String {
    fn from(value: IdempotencyKey) -> Self {
        value.0
    }
}

/// Opaque delegation proof reference the caller presents (08 §4.4, D-111).
///
/// Orders forwards it unvalidated to the PDP and records it as *supplied*; it never verifies a
/// signature, expiry, scope or revocation and it is never authorization evidence by itself.
/// Bounded printable ASCII; Debug output never logs the value.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DelegationProofRef(String);
impl DelegationProofRef {
    /// Maximum accepted reference length in bytes.
    pub const MAX_LEN: usize = 512;
    /// The opaque reference text, for PDP request context and evidence rows only.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for DelegationProofRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DelegationProofRef([redacted])")
    }
}
impl TryFrom<String> for DelegationProofRef {
    type Error = InvalidBoundaryValue;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty()
            || value.len() > Self::MAX_LEN
            || !value.bytes().all(|b| (33..=126).contains(&b))
        {
            return Err(InvalidBoundaryValue);
        }
        Ok(Self(value))
    }
}
impl From<DelegationProofRef> for String {
    fn from(value: DelegationProofRef) -> Self {
        value.0
    }
}

/// Trusted adapter metadata, separate from the authored contribution and caller context.
#[derive(Debug, Clone)]
pub struct CallMeta {
    pub expected_version: OrderVersion,
    pub idempotency_key: IdempotencyKey,
    pub correlation_id: Option<Uuid>,
    /// Supplied delegation proof reference, forwarded to every PDP decision of the call.
    pub delegation_proof_ref: Option<DelegationProofRef>,
}

/// Existing transition result. Commercial assessment fields land with S3's native codec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransitionResult {
    pub order_id: Uuid,
    pub state: OrderState,
    pub version: OrderVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_revision: Option<DraftRevision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_audit_id: Option<Uuid>,
    /// The server-reserved identity of an inserted line (line `POST` only; 02 *Author Line*).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_id: Option<Uuid>,
}

/// Server-issued D-198 grant; a generation never substitutes for a commercial version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnSignalResult {
    pub transition: TransitionResult,
    #[serde(with = "time::serde::rfc3339")]
    pub spawn_signal_at: time::OffsetDateTime,
    pub grant_id: Uuid,
    pub generation: u64,
}
