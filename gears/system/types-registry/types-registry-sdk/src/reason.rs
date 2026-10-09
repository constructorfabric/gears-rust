//! Wire `reason` vocabulary of types-registry canonical errors, grouped by the canonical
//! category whose `ctx.reason` carries it (ADR 0005 rule 4).
//!
//! Field-violation reasons live in [`crate::field`], precondition `type`s in
//! [`crate::precondition`], and an item's admission reason in [`crate::item_failure`].

/// Values for `CanonicalError::Aborted.ctx.reason`.
pub mod aborted {
    use core::fmt;

    /// A mutation was accepted, but reading its operation back failed. `resource_name` is the
    /// operation UUID; retrying with the same idempotency key replays it (SPEC D19).
    pub const OPERATION_READ_FAILED: &str = "OPERATION_READ_FAILED";

    /// Typed view of an `Aborted` reason.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum AbortReason {
        /// See [`OPERATION_READ_FAILED`].
        OperationReadFailed,
        /// Unmodeled reason; preserves the raw wire string.
        Unknown(String),
    }

    impl AbortReason {
        /// Project `Aborted.ctx.reason`; an unmodeled value is preserved in `Unknown`.
        #[must_use]
        pub fn from_wire(s: &str) -> Self {
            match s {
                OPERATION_READ_FAILED => Self::OperationReadFailed,
                other => Self::Unknown(other.to_owned()),
            }
        }

        /// Render back to the wire reason. Inverse of [`Self::from_wire`].
        #[must_use]
        pub fn as_wire(&self) -> &str {
            match self {
                Self::OperationReadFailed => OPERATION_READ_FAILED,
                Self::Unknown(s) => s.as_str(),
            }
        }
    }

    impl fmt::Display for AbortReason {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.as_wire())
        }
    }
}
