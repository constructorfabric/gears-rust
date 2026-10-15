//! Machine-readable reason codes carried by durable-execution `CanonicalError`s.
//!
//! Callers decide retries by category: `Aborted` (`LEASE_LOST`,
//! `CONCURRENT_UPDATE`) may be retried after re-reading state,
//! `FailedPrecondition` and `AlreadyExists` must not be retried unchanged, and
//! `ServiceUnavailable` should be retried with backoff.

/// `PermissionDenied` reason: the PDP or subject checks rejected the call.
pub const EXECUTION_ACCESS_DENIED: &str = "EXECUTION_ACCESS_DENIED";
/// `Aborted` reason: the caller no longer owns the claim or lease.
pub const LEASE_LOST: &str = "LEASE_LOST";
/// `Aborted` reason: an epoch, revision or row version changed concurrently.
pub const CONCURRENT_UPDATE: &str = "CONCURRENT_UPDATE";

/// `FailedPrecondition` violation type: the run or registration state forbids the transition.
pub const INVALID_STATE: &str = "INVALID_STATE";
/// `FailedPrecondition` violation type: the definition generation is retired or revoked.
pub const DEFINITION_INACTIVE: &str = "DEFINITION_INACTIVE";
/// `FailedPrecondition` violation type: the stored run was pinned to a different definition contract.
pub const DEFINITION_MISMATCH: &str = "DEFINITION_MISMATCH";

/// `InvalidArgument` field-violation reason: the execution contract is malformed.
pub const INVALID_DEFINITION: &str = "INVALID_DEFINITION";
/// `InvalidArgument` field-violation reason: a value has an unsupported format.
pub const INVALID_FORMAT: &str = "INVALID_FORMAT";
/// `InvalidArgument` field-violation reason: the payload exceeds its size limit.
pub const PAYLOAD_TOO_LARGE: &str = "PAYLOAD_TOO_LARGE";
/// `InvalidArgument` field-violation reason: a value is outside its allowed range.
pub const OUT_OF_RANGE: &str = "OUT_OF_RANGE";
