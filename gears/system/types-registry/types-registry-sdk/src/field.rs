//! Wire `field` / `reason` vocabulary for field violations under
//! [`CanonicalError::InvalidArgument`].
//!
//! Types-registry emits exactly three `InvalidArgument` field-violation
//! `reason` codes, each attributed to a fixed request `field`:
//!
//! | `reason` | `field` | source |
//! |---|---|---|
//! | [`INVALID_GTS_ID`] | [`GTS_ID_FIELD`] | a GTS id failed to parse / kind-mismatched |
//! | [`INVALID_QUERY`] | [`QUERY_FIELD`] | a list/query pattern was out-of-spec |
//! | [`VALIDATION_FAILED`] | [`ENTITY_FIELD`] | entity content failed schema validation |
//!
//! The `reason` slot is the dispatch discriminator, so it is fanned into the
//! typed [`ValidationReason`] sub-enum (consumers match the variant rather than
//! the wire string). The `field` slot stays a free `String` on the projection —
//! it is an attribution identifier, not a discriminator.
//!
//! The impl crate's single `From<DomainError> for CanonicalError` ladder
//! references the same constants at construction time so the SDK vocabulary and
//! the wire can never drift — the round-trip tests in [`crate::error`] pin every
//! constant to its `Problem` JSON path.
//!
//! [`CanonicalError::InvalidArgument`]: toolkit_canonical_errors::CanonicalError::InvalidArgument

// ---------------------------------------------------------------------------
// `field_violations[].field` attribution keys.
//
// Identify *which* request field failed. Extracted to consts (ADR 0005 Rule 6)
// so the impl ladder and the SDK vocabulary cannot drift; pinned by the
// round-trip tests.
// ---------------------------------------------------------------------------

/// The GTS identifier field (carries [`INVALID_GTS_ID`]).
pub const GTS_ID_FIELD: &str = "gts_id";

/// The list/query pattern field (carries [`INVALID_QUERY`]).
pub const QUERY_FIELD: &str = "query";

/// The entity-content field (carries [`VALIDATION_FAILED`]).
pub const ENTITY_FIELD: &str = "entity";

// ---------------------------------------------------------------------------
// `field_violations[].reason` codes.
// ---------------------------------------------------------------------------

/// The string is not a valid GTS identifier (parse failure or kind mismatch).
pub const INVALID_GTS_ID: &str = "INVALID_GTS_ID";

/// The list/query parameters are syntactically invalid (out-of-spec wildcard).
pub const INVALID_QUERY: &str = "INVALID_QUERY";

/// The entity content failed schema validation.
pub const VALIDATION_FAILED: &str = "VALIDATION_FAILED";

// ---------------------------------------------------------------------------
// Synthetic shape sentinels (projection-side, NOT wire `reason` codes).
//
// types-registry never emits these — it only produces the `FieldViolations`
// shape. They tag the *other* `InvalidArgument` shapes (`Format` / `Constraint`)
// so the projection keeps a distinct, non-empty discriminator instead of
// collapsing both into an ambiguous `Unknown("")`. Rendered by
// [`ValidationReason::as_wire`] for diagnostics; never returned by
// [`ValidationReason::from_wire`] (no `field_violations[].reason` carries them).
// ---------------------------------------------------------------------------

/// Diagnostic token for [`ValidationReason::Format`].
pub const FORMAT_SHAPE: &str = "FORMAT";

/// Diagnostic token for [`ValidationReason::Constraint`].
pub const CONSTRAINT_SHAPE: &str = "CONSTRAINT";

// ---------------------------------------------------------------------------
// Typed view of the `field_violations[].reason` codes.
// ---------------------------------------------------------------------------

/// Typed view of the types-registry `InvalidArgument` `reason` strings
/// declared above.
///
/// Carried by each [`crate::error::FieldIssue::reason`].
/// [`from_wire`](Self::from_wire) returns `Self` (not `Option`) with an
/// [`Self::Unknown`] catch-all because every `reason` types-registry emits is
/// one of the modeled values — the catch-all only fires for a future reason,
/// keeping the projection forward-compatible.
///
/// [`Self::Format`] and [`Self::Constraint`] are synthetic projection-side
/// sentinels for the non-field-violation `InvalidArgument` shapes; they are
/// produced by the projection, never by [`from_wire`](Self::from_wire).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationReason {
    /// See [`INVALID_GTS_ID`].
    InvalidGtsId,
    /// See [`INVALID_QUERY`].
    InvalidQuery,
    /// See [`VALIDATION_FAILED`].
    ValidationFailed,
    /// Synthetic sentinel for a canonical `InvalidArgument` carried as the
    /// `Format` shape (a single format descriptor) rather than per-field
    /// violations. Renders the reserved token [`FORMAT_SHAPE`]. Not produced
    /// by [`Self::from_wire`]; types-registry does not emit this shape today.
    Format,
    /// Synthetic sentinel for a canonical `InvalidArgument` carried as the
    /// `Constraint` shape. Renders the reserved token [`CONSTRAINT_SHAPE`].
    /// Same rationale as [`Self::Format`].
    Constraint,
    /// Unmodeled / future reason — preserves the raw wire string.
    Unknown(String),
}

impl ValidationReason {
    /// Project a wire `field_violations[].reason` string into the typed
    /// discriminator. Any unmodeled value is preserved in [`Self::Unknown`].
    #[must_use]
    pub fn from_wire(s: &str) -> Self {
        match s {
            INVALID_GTS_ID => Self::InvalidGtsId,
            INVALID_QUERY => Self::InvalidQuery,
            VALIDATION_FAILED => Self::ValidationFailed,
            other => Self::Unknown(other.to_owned()),
        }
    }

    /// Render the discriminator to its wire `reason` string. Inverse of
    /// [`Self::from_wire`] for the wire-backed variants; the synthetic
    /// [`Self::Format`] / [`Self::Constraint`] sentinels render their reserved
    /// diagnostic tokens ([`FORMAT_SHAPE`] / [`CONSTRAINT_SHAPE`]).
    #[must_use]
    pub fn as_wire(&self) -> &str {
        match self {
            Self::InvalidGtsId => INVALID_GTS_ID,
            Self::InvalidQuery => INVALID_QUERY,
            Self::ValidationFailed => VALIDATION_FAILED,
            Self::Format => FORMAT_SHAPE,
            Self::Constraint => CONSTRAINT_SHAPE,
            Self::Unknown(s) => s.as_str(),
        }
    }
}

impl core::fmt::Display for ValidationReason {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_wire())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_reason_round_trips_each_constant() {
        for (wire, expected) in [
            (INVALID_GTS_ID, ValidationReason::InvalidGtsId),
            (INVALID_QUERY, ValidationReason::InvalidQuery),
            (VALIDATION_FAILED, ValidationReason::ValidationFailed),
        ] {
            assert_eq!(ValidationReason::from_wire(wire), expected);
            assert_eq!(expected.as_wire(), wire);
        }
    }

    #[test]
    fn validation_reason_preserves_unknown_wire_string() {
        let raw = "FUTURE_REASON";
        let r = ValidationReason::from_wire(raw);
        assert_eq!(r, ValidationReason::Unknown(raw.to_owned()));
        assert_eq!(r.as_wire(), raw);
    }

    #[test]
    fn shape_sentinels_render_distinct_non_empty_tokens() {
        assert_eq!(ValidationReason::Format.as_wire(), FORMAT_SHAPE);
        assert_eq!(ValidationReason::Constraint.as_wire(), CONSTRAINT_SHAPE);
        // The whole point of the fix: the two shapes are no longer the same
        // ambiguous `Unknown("")` — they are distinct, non-empty discriminators.
        assert_ne!(ValidationReason::Format, ValidationReason::Constraint);
        assert!(!ValidationReason::Format.as_wire().is_empty());
        assert!(!ValidationReason::Constraint.as_wire().is_empty());
    }

    #[test]
    fn from_wire_never_yields_synthetic_shape_sentinels() {
        // The shape sentinels are projection-side only; feeding their tokens
        // back through `from_wire` yields `Unknown`, not the sentinel variant.
        assert_eq!(
            ValidationReason::from_wire(FORMAT_SHAPE),
            ValidationReason::Unknown(FORMAT_SHAPE.to_owned())
        );
        assert_eq!(
            ValidationReason::from_wire(CONSTRAINT_SHAPE),
            ValidationReason::Unknown(CONSTRAINT_SHAPE.to_owned())
        );
    }
}
