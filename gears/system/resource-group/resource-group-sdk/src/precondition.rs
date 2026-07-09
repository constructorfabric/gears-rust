//! Wire `subject` / `type` vocabulary for precondition violations under
//! [`CanonicalError::FailedPrecondition`].
//!
//! Each `*_SUBJECT` constant is the stable `subject` discriminator that
//! lands in `CanonicalError::FailedPrecondition.ctx.violations[].subject`
//! — the field consumers dispatch on. Resource-group emits several
//! distinct precondition families that **all** share the
//! `FailedPrecondition` category and the single `STATE` wire `type`
//! ([`STATE_TYPE`]); the **subject** is therefore the discriminator, not
//! the type. Two subjects carry domain meaning consumers care about:
//!
//! * [`Subject::Hierarchy`] (`"hierarchy"`) ⇒ the write would create a
//!   **cycle** in the group hierarchy.
//! * [`Subject::Limit`] (`"limit"`) ⇒ a configured **limit** (depth,
//!   width, …) would be exceeded.
//!
//! The impl crate's single `From<DomainError> for CanonicalError` ladder
//! references these constants; the round-trip tests in [`crate::error`]
//! pin each to its `Problem` JSON path.
//!
//! [`CanonicalError::FailedPrecondition`]: toolkit_canonical_errors::CanonicalError::FailedPrecondition

use core::fmt;

// ---------------------------------------------------------------------------
// `violations[].subject` discriminators.
// ---------------------------------------------------------------------------

/// Removing allowed parents / disabling root placement would break
/// existing group-hierarchy relationships.
pub const ALLOWED_PARENTS_SUBJECT: &str = "allowed_parents";

/// The write would introduce a cycle in the group hierarchy
/// (cycle-detected).
pub const HIERARCHY_SUBJECT: &str = "hierarchy";

/// A type still has groups of this type (active references) and cannot
/// be deleted.
pub const ACTIVE_REFERENCES_SUBJECT: &str = "active_references";

/// A configured limit (depth, width, …) would be exceeded
/// (limit-violation).
pub const LIMIT_SUBJECT: &str = "limit";

/// A cross-tenant link would be created — a resource may belong to
/// groups of a single tenant only.
pub const TENANT_SUBJECT: &str = "tenant";

// ---------------------------------------------------------------------------
// `violations[].type` token.
//
// Resource-group uses a single, uniform `type` across every precondition
// family — the dispatch discriminator is the subject above, not the type.
// Extracted to a const (ADR 0005 Rule 6) so the impl ladder and SDK
// vocabulary cannot drift; pinned by the round-trip tests.
// ---------------------------------------------------------------------------

/// The uniform `violations[].type` token resource-group emits for every
/// precondition family.
pub const STATE_TYPE: &str = "STATE";

// ---------------------------------------------------------------------------
// Typed view of the `violations[].subject` discriminators.
// ---------------------------------------------------------------------------

/// Typed view of the resource-group `FailedPrecondition` `subject`
/// strings declared above.
///
/// Carried by [`crate::ResourceGroupError::FailedPrecondition::subject`].
/// `from_wire` returns `Self` (not `Option`) with an [`Self::Unknown`]
/// catch-all because every `subject` resource-group emits under
/// `FailedPrecondition` is one of the modeled values — the catch-all only
/// fires for a future subject, keeping the projection forward-compatible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    /// See [`ALLOWED_PARENTS_SUBJECT`].
    AllowedParents,
    /// See [`HIERARCHY_SUBJECT`] — denotes a detected cycle.
    Hierarchy,
    /// See [`ACTIVE_REFERENCES_SUBJECT`].
    ActiveReferences,
    /// See [`LIMIT_SUBJECT`] — denotes a configured-limit violation.
    Limit,
    /// See [`TENANT_SUBJECT`].
    Tenant,
    /// Unmodeled / future subject — preserves the raw wire string.
    Unknown(String),
}

impl Subject {
    /// Project a wire `violations[].subject` string into the typed
    /// discriminator. Any unmodeled value is preserved in
    /// [`Self::Unknown`].
    #[must_use]
    pub fn from_wire(s: &str) -> Self {
        match s {
            ALLOWED_PARENTS_SUBJECT => Self::AllowedParents,
            HIERARCHY_SUBJECT => Self::Hierarchy,
            ACTIVE_REFERENCES_SUBJECT => Self::ActiveReferences,
            LIMIT_SUBJECT => Self::Limit,
            TENANT_SUBJECT => Self::Tenant,
            other => Self::Unknown(other.to_owned()),
        }
    }

    /// Render the discriminator back to its wire `subject` string.
    /// Inverse of [`Self::from_wire`] for the modeled variants.
    #[must_use]
    pub fn as_wire(&self) -> &str {
        match self {
            Self::AllowedParents => ALLOWED_PARENTS_SUBJECT,
            Self::Hierarchy => HIERARCHY_SUBJECT,
            Self::ActiveReferences => ACTIVE_REFERENCES_SUBJECT,
            Self::Limit => LIMIT_SUBJECT,
            Self::Tenant => TENANT_SUBJECT,
            Self::Unknown(s) => s.as_str(),
        }
    }
}

impl fmt::Display for Subject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_wire())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subject_round_trips_each_constant() {
        for (wire, expected) in [
            (ALLOWED_PARENTS_SUBJECT, Subject::AllowedParents),
            (HIERARCHY_SUBJECT, Subject::Hierarchy),
            (ACTIVE_REFERENCES_SUBJECT, Subject::ActiveReferences),
            (LIMIT_SUBJECT, Subject::Limit),
            (TENANT_SUBJECT, Subject::Tenant),
        ] {
            assert_eq!(Subject::from_wire(wire), expected);
            assert_eq!(expected.as_wire(), wire);
        }
    }

    #[test]
    fn subject_preserves_unknown_wire_string() {
        let raw = "future_subject";
        let s = Subject::from_wire(raw);
        assert_eq!(s, Subject::Unknown(raw.to_owned()));
        assert_eq!(s.as_wire(), raw);
    }
}
