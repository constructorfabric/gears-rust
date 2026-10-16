//! Wire `reason` discriminator vocabulary for credstore canonical errors.
//!
//! Consumers dispatch on these constants to handle specific credstore
//! sub-categories without string-typing literals at the call site. The gear
//! will build its canonical errors from these constants when the gear code
//! lands (a later step); until then the round-trip tests in `crate::error`
//! pin each value to its JSON path.
//!
//! Constants are grouped by the canonical category whose context carries them:
//!
//! * [`aborted`] — `CanonicalError::Aborted.ctx.reason`
//! * [`failed_precondition`] — `CanonicalError::FailedPrecondition.ctx.violations[].type`
//! * [`permission_denied`] — `CanonicalError::PermissionDenied.ctx.reason`
//!
//! For field-violation reasons (`CanonicalError::InvalidArgument`) see
//! [`crate::field`].
//!
//! ## Example
//!
//! ```ignore
//! use credstore_v2_sdk::reason;
//! use toolkit_canonical_errors::CanonicalError;
//!
//! match err {
//!     CanonicalError::Aborted { ctx, .. } => match ctx.reason.as_str() {
//!         reason::aborted::OPTIMISTIC_LOCK_FAILURE => /* re-read and retry */,
//!         _ => /* type conflict or unknown */,
//!     },
//!     _ => /* other */,
//! }
//! ```

/// Wire `reason` values for [`CanonicalError::Aborted`].
///
/// [`CanonicalError::Aborted`]: toolkit_canonical_errors::CanonicalError::Aborted
pub mod aborted {
    use core::fmt;

    /// A write precondition no longer holds: a replace (`Exists` / `Matches`)
    /// whose target is absent, or a `Matches` validator that is stale (lost
    /// compare-and-set). Re-read the record and retry.
    pub const OPTIMISTIC_LOCK_FAILURE: &str = "OPTIMISTIC_LOCK_FAILURE";

    /// A replace or `patch` names a secret type differing from the stored one.
    pub const TYPE_IMMUTABLE: &str = "TYPE_IMMUTABLE";

    /// A create over a reference that resolves, for the creating caller, to an
    /// ancestor's `shared` record of a different type.
    pub const TYPE_MISMATCH_WITH_INHERITED: &str = "TYPE_MISMATCH_WITH_INHERITED";

    /// A create over a reference a descendant tenant already holds as a
    /// non-private record of a different type.
    pub const TYPE_MISMATCH_WITH_DESCENDANT: &str = "TYPE_MISMATCH_WITH_DESCENDANT";

    /// Typed view of every `Aborted.ctx.reason` the gear emits.
    ///
    /// The three `TypeImmutable` / `TypeMismatchWith*` variants are the secret
    /// type conflicts carried by [`crate::CredStoreError::TypeConflict`].
    ///
    /// [`Self::Unknown`] preserves any value this SDK does not model.
    #[derive(Debug, Clone, PartialEq, Eq)]
    #[non_exhaustive]
    pub enum Reason {
        /// See [`OPTIMISTIC_LOCK_FAILURE`].
        OptimisticLockFailure,
        /// See [`TYPE_IMMUTABLE`].
        TypeImmutable,
        /// See [`TYPE_MISMATCH_WITH_INHERITED`].
        TypeMismatchWithInherited,
        /// See [`TYPE_MISMATCH_WITH_DESCENDANT`].
        TypeMismatchWithDescendant,
        /// Unmodeled reason; preserves the raw wire string.
        Unknown(String),
    }

    impl Reason {
        /// Project a wire `Aborted.ctx.reason` string into the typed
        /// discriminator.
        #[must_use]
        pub fn from_wire(s: &str) -> Self {
            match s {
                OPTIMISTIC_LOCK_FAILURE => Self::OptimisticLockFailure,
                TYPE_IMMUTABLE => Self::TypeImmutable,
                TYPE_MISMATCH_WITH_INHERITED => Self::TypeMismatchWithInherited,
                TYPE_MISMATCH_WITH_DESCENDANT => Self::TypeMismatchWithDescendant,
                other => Self::Unknown(other.to_owned()),
            }
        }

        /// Render the discriminator back to its wire string.
        #[must_use]
        pub fn as_wire(&self) -> &str {
            match self {
                Self::OptimisticLockFailure => OPTIMISTIC_LOCK_FAILURE,
                Self::TypeImmutable => TYPE_IMMUTABLE,
                Self::TypeMismatchWithInherited => TYPE_MISMATCH_WITH_INHERITED,
                Self::TypeMismatchWithDescendant => TYPE_MISMATCH_WITH_DESCENDANT,
                Self::Unknown(s) => s.as_str(),
            }
        }
    }

    impl fmt::Display for Reason {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.as_wire())
        }
    }
}

/// Wire reason values for [`CanonicalError::FailedPrecondition`]. They travel
/// in `ctx.violations[].type` of the precondition violation.
///
/// [`CanonicalError::FailedPrecondition`]: toolkit_canonical_errors::CanonicalError::FailedPrecondition
pub mod failed_precondition {
    use core::fmt;

    /// The record resolved and the caller may read its secret, but the
    /// decisive `active` record's `expires_at` has passed. Renew the record
    /// (`patch` of `expires_at` or a new `secret`) to restore the secret.
    pub const SECRET_EXPIRED: &str = "SECRET_EXPIRED";

    /// A `sharing` change would move the record across the private /
    /// tenant-shared boundary.
    pub const UNSUPPORTED_TRANSITION: &str = "UNSUPPORTED_TRANSITION";

    /// Typed view of every `FailedPrecondition` reason the gear emits.
    ///
    /// [`Self::Unknown`] preserves any value this SDK does not model.
    #[derive(Debug, Clone, PartialEq, Eq)]
    #[non_exhaustive]
    pub enum Reason {
        /// See [`SECRET_EXPIRED`].
        SecretExpired,
        /// See [`UNSUPPORTED_TRANSITION`].
        UnsupportedTransition,
        /// Unmodeled reason; preserves the raw wire string.
        Unknown(String),
    }

    impl Reason {
        /// Project a wire precondition-violation `type` string into the typed
        /// discriminator.
        #[must_use]
        pub fn from_wire(s: &str) -> Self {
            match s {
                SECRET_EXPIRED => Self::SecretExpired,
                UNSUPPORTED_TRANSITION => Self::UnsupportedTransition,
                other => Self::Unknown(other.to_owned()),
            }
        }

        /// Render the discriminator back to its wire string.
        #[must_use]
        pub fn as_wire(&self) -> &str {
            match self {
                Self::SecretExpired => SECRET_EXPIRED,
                Self::UnsupportedTransition => UNSUPPORTED_TRANSITION,
                Self::Unknown(s) => s.as_str(),
            }
        }
    }

    impl fmt::Display for Reason {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.as_wire())
        }
    }
}

/// Wire `reason` values for [`CanonicalError::PermissionDenied`].
///
/// [`CanonicalError::PermissionDenied`]: toolkit_canonical_errors::CanonicalError::PermissionDenied
pub mod permission_denied {
    use core::fmt;

    /// The caller lacks a required action (`read_secret`, `write`,
    /// `write_secret`, `delete`, …) on the resource.
    pub const ACCESS_DENIED: &str = "ACCESS_DENIED";

    /// Typed view of the `PermissionDenied.ctx.reason` values.
    ///
    /// [`Self::Unknown`] preserves any value this SDK does not model.
    #[derive(Debug, Clone, PartialEq, Eq)]
    #[non_exhaustive]
    pub enum Reason {
        /// See [`ACCESS_DENIED`].
        AccessDenied,
        /// Unmodeled reason; preserves the raw wire string.
        Unknown(String),
    }

    impl Reason {
        /// Project a wire `PermissionDenied.ctx.reason` string into the typed
        /// discriminator.
        #[must_use]
        pub fn from_wire(s: &str) -> Self {
            match s {
                ACCESS_DENIED => Self::AccessDenied,
                other => Self::Unknown(other.to_owned()),
            }
        }

        /// Render the discriminator back to its wire string.
        #[must_use]
        pub fn as_wire(&self) -> &str {
            match self {
                Self::AccessDenied => ACCESS_DENIED,
                Self::Unknown(s) => s.as_str(),
            }
        }
    }

    impl fmt::Display for Reason {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.as_wire())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aborted_reasons_round_trip() {
        for wire in [
            aborted::OPTIMISTIC_LOCK_FAILURE,
            aborted::TYPE_IMMUTABLE,
            aborted::TYPE_MISMATCH_WITH_INHERITED,
            aborted::TYPE_MISMATCH_WITH_DESCENDANT,
        ] {
            let typed = aborted::Reason::from_wire(wire);
            assert!(!matches!(typed, aborted::Reason::Unknown(_)), "{wire}");
            assert_eq!(typed.as_wire(), wire);
            assert_eq!(typed.to_string(), wire);
        }
        assert_eq!(
            aborted::Reason::from_wire("X"),
            aborted::Reason::Unknown("X".to_owned())
        );
    }

    #[test]
    fn failed_precondition_reasons_round_trip() {
        for wire in [
            failed_precondition::SECRET_EXPIRED,
            failed_precondition::UNSUPPORTED_TRANSITION,
        ] {
            let typed = failed_precondition::Reason::from_wire(wire);
            assert!(
                !matches!(typed, failed_precondition::Reason::Unknown(_)),
                "{wire}"
            );
            assert_eq!(typed.as_wire(), wire);
            assert_eq!(typed.to_string(), wire);
        }
        assert_eq!(
            failed_precondition::Reason::from_wire("X"),
            failed_precondition::Reason::Unknown("X".to_owned())
        );
    }

    #[test]
    fn permission_denied_reasons_round_trip() {
        let typed = permission_denied::Reason::from_wire(permission_denied::ACCESS_DENIED);
        assert_eq!(typed, permission_denied::Reason::AccessDenied);
        assert_eq!(typed.as_wire(), permission_denied::ACCESS_DENIED);
        assert_eq!(typed.to_string(), permission_denied::ACCESS_DENIED);
        assert_eq!(
            permission_denied::Reason::from_wire("X"),
            permission_denied::Reason::Unknown("X".to_owned())
        );
    }
}
