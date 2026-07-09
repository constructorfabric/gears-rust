//! Wire `field` / `reason` vocabulary for field violations under
//! [`CanonicalError::InvalidArgument`].
//!
//! Resource-group emits exactly one field-violation `reason`
//! ([`INVALID_PARENT_TYPE`]) on the [`crate::ResourceGroupError::InvalidArgument`]
//! variant, attributed to the [`PARENT_TYPE_FIELD`] field. Because there
//! is a single reason, it stays a **plain constant** with no typed
//! sub-enum (ADR 0005: single-value reasons stay consts) — the
//! projection surfaces `reason` as a raw `String` consumers match against
//! this constant. The other `InvalidArgument` shape resource-group emits
//! (generic `Validation`, mapped via `with_format`) carries no field
//! attribution.
//!
//! The impl crate's single `From<DomainError> for CanonicalError` ladder
//! references the same constants at construction time so the SDK
//! vocabulary and the wire can never drift — the round-trip tests in
//! [`crate::error`] pin every constant to its `Problem` JSON path.
//!
//! [`CanonicalError::InvalidArgument`]: toolkit_canonical_errors::CanonicalError::InvalidArgument

// ---------------------------------------------------------------------------
// `field_violations[].reason` code.
// ---------------------------------------------------------------------------

/// The requested parent type is not permitted by the type's
/// `allowed_parent_types` configuration.
pub const INVALID_PARENT_TYPE: &str = "INVALID_PARENT_TYPE";

// ---------------------------------------------------------------------------
// `field_violations[].field` attribution key.
//
// Identifies *which* request field failed. Extracted to a const
// (ADR 0005 Rule 6) so the impl ladder and the SDK vocabulary cannot
// drift; pinned by the round-trip tests.
// ---------------------------------------------------------------------------

/// `parent_type` reference field (carries [`INVALID_PARENT_TYPE`]).
pub const PARENT_TYPE_FIELD: &str = "parent_type";
