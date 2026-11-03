//! Wire vocabulary of the errors of [`LicenseEnforcementApi`](crate::LicenseEnforcementApi).
//!
//! Every method returns `CanonicalError`. The gear maps its domain errors to it
//! (DESIGN §3.3, Error model). These constants are the stable discriminators
//! that consumers match on:
//!
//! - `InvalidArgument` → `field_violations[].reason` from [`reason`];
//! - `FailedPrecondition` → `violations[].type` is [`PRECONDITION_TYPE`],
//!   `violations[].subject` from [`precondition`];
//! - `Aborted` → `reason` from [`aborted`];
//! - `Unimplemented` → the linked plugin does not serve the intake that the
//!   operation needs (`issue` or `import`). The binary cannot serve it at all,
//!   so no discriminator is needed.

/// `violations[].type` of every `FailedPrecondition` of this module.
pub const PRECONDITION_TYPE: &str = "STATE";

/// `reason` values of `Aborted`.
pub mod aborted {
    /// `expected_version` differs from the current version. Read the object
    /// again before a retry.
    pub const OPTIMISTIC_LOCK_FAILURE: &str = "OPTIMISTIC_LOCK_FAILURE";
    /// A concurrent admission changed the plugin material that `admit` read.
    /// Retry the request with the same idempotency token.
    pub const ADMISSION_CONFLICT: &str = "ADMISSION_CONFLICT";
}

/// `violations[].subject` values of `FailedPrecondition`.
pub mod precondition {
    /// The verb is not allowed from the current state, or the recipient is not
    /// a descendant of the holder.
    pub const INVALID_TRANSITION: &str = "invalid_transition";
    /// Replacement or supersession cannot carry a binding. One violation per
    /// binding; its `description` names the grant.
    pub const BINDING_WITHOUT_SUCCESSOR: &str = "binding_without_successor";
    /// The evidence that the imported evidence replaces is not in the store.
    pub const REPLACED_EVIDENCE_MISSING: &str = "replaced_evidence_missing";
    /// Other evidence already replaced the evidence that the imported evidence
    /// replaces.
    pub const EVIDENCE_ALREADY_REPLACED: &str = "evidence_already_replaced";
}

/// `field_violations[].reason` values of `InvalidArgument`.
pub mod reason {
    /// The type id is not registered.
    pub const TYPE_NOT_REGISTERED: &str = "TYPE_NOT_REGISTERED";
    /// The type does not derive from the licensing base type.
    pub const TYPE_NOT_DERIVED: &str = "TYPE_NOT_DERIVED";
    /// The subject type is not in `admitted_subjects` of the Resource contract.
    pub const SUBJECT_NOT_ADMITTED: &str = "SUBJECT_NOT_ADMITTED";
    /// Conditions or recipient rules fail the evaluator's validation.
    pub const RULES_INVALID: &str = "RULES_INVALID";
    /// A sub-grant would not be a subset of its parent.
    pub const NOT_A_SUBSET: &str = "NOT_A_SUBSET";
    /// The recipient fails the recipient rules.
    pub const RECIPIENT_REJECTED: &str = "RECIPIENT_REJECTED";
    /// The admission plugin rejected the evidence. One value per
    /// [`RejectionReason`](crate::RejectionReason); see
    /// [`RejectionReason::reason`](crate::RejectionReason::reason).
    pub const EVIDENCE_MALFORMED: &str = "EVIDENCE_MALFORMED";
    /// See [`EVIDENCE_MALFORMED`].
    pub const EVIDENCE_SIGNATURE_INVALID: &str = "EVIDENCE_SIGNATURE_INVALID";
    /// See [`EVIDENCE_MALFORMED`].
    pub const EVIDENCE_KEY_UNTRUSTED: &str = "EVIDENCE_KEY_UNTRUSTED";
    /// See [`EVIDENCE_MALFORMED`].
    pub const EVIDENCE_STALE: &str = "EVIDENCE_STALE";
    /// See [`EVIDENCE_MALFORMED`].
    pub const EVIDENCE_CLOCK_ROLLBACK: &str = "EVIDENCE_CLOCK_ROLLBACK";
    /// See [`EVIDENCE_MALFORMED`].
    pub const EVIDENCE_REPLAY: &str = "EVIDENCE_REPLAY";
    /// See [`EVIDENCE_MALFORMED`].
    pub const EVIDENCE_VERSION_REGRESSION: &str = "EVIDENCE_VERSION_REGRESSION";
    /// The check-explain request does not conform to its contracts.
    pub const REQUEST_NONCONFORMING: &str = "REQUEST_NONCONFORMING";
}
