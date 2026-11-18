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
//! - `AlreadyExists` → `resource` is `idempotency_token` for a token reused
//!   with another payload, else the id of a root grant that the evidence names
//!   and the gear already stores;
//! - `Unimplemented` → the binary cannot serve the operation at all, so no
//!   discriminator is needed: the linked plugin does not serve the intake that
//!   it needs (`issue` or `import`), `renew` meets a plugin that verifies
//!   evidence, or the operation is not implemented yet (the gear README lists
//!   these).

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
    /// A concurrent replacement moved a tree under a new root while the
    /// request waited for it. Retry the request with the same idempotency
    /// token.
    pub const TREE_MOVED: &str = "TREE_MOVED";
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
    /// The resource type is abstract: no check names it, so a grant on it
    /// would cover nothing.
    pub const TYPE_ABSTRACT: &str = "TYPE_ABSTRACT";
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
    /// A required field is empty, for example no grant to issue.
    pub const REQUIRED: &str = "REQUIRED";
    /// The window does not satisfy `not_before < expires_at <= grace_until`,
    /// has ended, or has a time outside years 0..=9999 in UTC.
    pub const INVALID_WINDOW: &str = "INVALID_WINDOW";
    /// The holder is not the caller's tenant or its descendant.
    pub const HOLDER_NOT_IN_SUBTREE: &str = "HOLDER_NOT_IN_SUBTREE";
    /// An id or a token contains a NUL character.
    pub const INVALID_CHARACTER: &str = "INVALID_CHARACTER";
    /// A request names a grant outside the grants that it acts on: a key of
    /// `expected_versions` outside `grant_ids`, or a successor that names a
    /// grant the replacement does not replace.
    pub const NOT_IN_REQUEST: &str = "NOT_IN_REQUEST";
    /// The members of a pack differ in `requires_acceptance`: a pack is
    /// accepted or rejected as a whole.
    pub const MIXED_ACCEPTANCE: &str = "MIXED_ACCEPTANCE";
    /// A list names the same item twice.
    pub const DUPLICATE: &str = "DUPLICATE";
    /// A field is over its limit in [`limits`](crate::limits).
    pub const TOO_LARGE: &str = "TOO_LARGE";
    /// The idempotency token already issued grants, and its idempotency window
    /// is over: the first result is gone. Retry with a new token.
    pub const TOKEN_REUSED: &str = "TOKEN_REUSED";
}
