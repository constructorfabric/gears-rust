//! Wire `reason` vocabulary of types-registry canonical errors, grouped by the canonical
//! category whose `ctx.reason` carries it.
//!
//! Field-violation reasons live in [`crate::field`], precondition `type`s in
//! [`crate::precondition`], and an item's admission reason in [`crate::item_failure`].

/// Values for `CanonicalError::Aborted.ctx.reason`.
pub mod aborted {
    use core::fmt;

    /// A mutation was accepted, but reading its operation back failed. `resource_name` is the
    /// operation UUID; retrying with the same idempotency key replays it.
    pub const OPERATION_READ_FAILED: &str = "OPERATION_READ_FAILED";

    /// SDK-detected protocol fault with a safe public detail.
    pub const RECONCILIATION_PROTOCOL_ERROR: &str = "RECONCILIATION_PROTOCOL_ERROR";

    /// Typed view of an `Aborted` reason.
    #[derive(Debug, Clone, PartialEq, Eq)]
    #[non_exhaustive]
    pub enum AbortReason {
        /// See [`OPERATION_READ_FAILED`].
        OperationReadFailed,
        /// See [`RECONCILIATION_PROTOCOL_ERROR`].
        ReconciliationProtocolError,
        /// Unmodeled reason; preserves the raw wire string.
        Unknown(String),
    }

    impl AbortReason {
        /// Project `Aborted.ctx.reason`; an unmodeled value is preserved in `Unknown`.
        #[must_use]
        pub fn from_wire(s: &str) -> Self {
            match s {
                OPERATION_READ_FAILED => Self::OperationReadFailed,
                RECONCILIATION_PROTOCOL_ERROR => Self::ReconciliationProtocolError,
                other => Self::Unknown(other.to_owned()),
            }
        }

        /// Render back to the wire reason. Inverse of [`Self::from_wire`].
        #[must_use]
        pub fn as_wire(&self) -> &str {
            match self {
                Self::OperationReadFailed => OPERATION_READ_FAILED,
                Self::ReconciliationProtocolError => RECONCILIATION_PROTOCOL_ERROR,
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
