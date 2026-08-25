//! Wire `reason` vocabulary for the single-valued AM canonical
//! categories — [`CanonicalError::Aborted`] and
//! [`CanonicalError::PermissionDenied`].
//!
//! These categories each carry a `ctx.reason` discriminator, but AM
//! emits only a fixed handful of values per category, so — unlike the
//! multi-valued [`crate::field`] / [`crate::precondition`] families —
//! they stay **plain constants** with no typed sub-enum (ADR 0005:
//! single-value reasons stay consts). Consumers match the projection's
//! `reason: String` field against these constants.
//!
//! The impl crate's single `From<DomainError> for CanonicalError` ladder
//! references the same constants; the round-trip tests in
//! [`crate::error`] pin each to its `Problem` JSON path.
//!
//! [`CanonicalError::Aborted`]: toolkit_canonical_errors::CanonicalError::Aborted
//! [`CanonicalError::PermissionDenied`]: toolkit_canonical_errors::CanonicalError::PermissionDenied

/// Wire `reason` values for [`CanonicalError::Aborted`] (HTTP 409).
///
/// [`CanonicalError::Aborted`]: toolkit_canonical_errors::CanonicalError::Aborted
pub mod aborted {
    /// `upsert_metadata` optimistic-lock precondition (the supplied
    /// `expected_version` did not match the stored row). The caller
    /// must re-read and re-issue with the updated `expected_version`.
    pub const METADATA_VERSION_MISMATCH: &str = "METADATA_VERSION_MISMATCH";

    /// Storage serializable-transaction retry budget was exhausted; the
    /// operation may succeed on a fresh retry.
    pub const SERIALIZATION_CONFLICT: &str = "SERIALIZATION_CONFLICT";

    /// A service-account call ended with the `IdP` outcome uncertain —
    /// the provider may have retained state. Unlike
    /// [`SERIALIZATION_CONFLICT`] this is **not** a retry-the-same
    /// signal: the caller reconciles by listing the tenant's accounts
    /// and matching the name it submitted, then either rotates that
    /// entry's secret or revokes it and provisions again.
    pub const AMBIGUOUS_OUTCOME: &str = "AMBIGUOUS_OUTCOME";
}

/// Wire `reason` values for [`CanonicalError::PermissionDenied`]
/// (HTTP 403).
///
/// [`CanonicalError::PermissionDenied`]: toolkit_canonical_errors::CanonicalError::PermissionDenied
pub mod permission {
    /// PEP denied cross-tenant access (or the AM-side ancestry walk
    /// rejected the call). The single denial reason AM emits.
    pub const CROSS_TENANT_DENIED: &str = "CROSS_TENANT_DENIED";
}
