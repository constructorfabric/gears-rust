//! Wire `reason` vocabulary for the single-valued resource-group
//! canonical categories — [`CanonicalError::Aborted`] and
//! [`CanonicalError::PermissionDenied`].
//!
//! These categories each carry a `ctx.reason` discriminator, but
//! resource-group emits only a single value per category, so — unlike the
//! multi-valued [`crate::precondition`] family — they stay **plain
//! constants** with no typed sub-enum (ADR 0005: single-value reasons
//! stay consts). Consumers match the projection's `reason: String` field
//! against these constants.
//!
//! The impl crate's single `From<DomainError> for CanonicalError` ladder
//! references the same constants; the round-trip tests in
//! [`crate::error`] pin each to its `Problem` JSON path.
//!
//! [`CanonicalError::Aborted`]: toolkit_canonical_errors::CanonicalError::Aborted
//! [`CanonicalError::PermissionDenied`]: toolkit_canonical_errors::CanonicalError::PermissionDenied

/// Wire `reason` value for [`CanonicalError::Aborted`] (HTTP 409).
///
/// [`CanonicalError::Aborted`]: toolkit_canonical_errors::CanonicalError::Aborted
pub mod aborted {
    /// Generic concurrency / state conflict not tied to a structural
    /// resource id (e.g. a write lost a race). The single abort reason
    /// resource-group emits.
    pub const CONFLICT: &str = "CONFLICT";
}

/// Wire `reason` value for [`CanonicalError::PermissionDenied`]
/// (HTTP 403).
///
/// [`CanonicalError::PermissionDenied`]: toolkit_canonical_errors::CanonicalError::PermissionDenied
pub mod permission {
    /// The authorization policy (PDP) denied the operation. The single
    /// denial reason resource-group emits.
    pub const ACCESS_DENIED: &str = "ACCESS_DENIED";
}
