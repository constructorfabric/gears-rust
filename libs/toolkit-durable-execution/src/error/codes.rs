//! Values of [`Error::code`](crate::Error::code). They are part of the stable contract:
//! never change a value, only add new ones.

/// Authorization rejected the call, or the caller has no subject or tenant.
pub const EXECUTION_ACCESS_DENIED: &str = "EXECUTION_ACCESS_DENIED";
/// No visible run has the requested id.
pub const RUN_NOT_FOUND: &str = "RUN_NOT_FOUND";
/// No definition is registered under the requested name.
pub const DEFINITION_NOT_FOUND: &str = "DEFINITION_NOT_FOUND";
/// The caller no longer owns the claim or lease.
pub const LEASE_LOST: &str = "LEASE_LOST";
/// An epoch, revision or row version changed concurrently.
pub const CONCURRENT_UPDATE: &str = "CONCURRENT_UPDATE";
/// The run or registration state forbids the transition.
pub const INVALID_STATE: &str = "INVALID_STATE";
/// The definition generation is retired or revoked.
pub const DEFINITION_INACTIVE: &str = "DEFINITION_INACTIVE";
/// This engine has no delivery and cannot schedule work.
pub const DELIVERY_DISABLED: &str = "DELIVERY_DISABLED";
/// The run or checkpoint belongs to a different definition contract.
pub const DEFINITION_MISMATCH: &str = "DEFINITION_MISMATCH";
/// A different contract, or a duplicate binding, uses the same definition name.
pub const DEFINITION_CONFLICT: &str = "DEFINITION_CONFLICT";
/// An idempotency or coalescing key was reused with different input.
pub const REQUEST_KEY_CONFLICT: &str = "REQUEST_KEY_CONFLICT";
/// The workflow definition or contract is malformed.
pub const INVALID_DEFINITION: &str = "INVALID_DEFINITION";
/// A request value has an unsupported format.
pub const INVALID_FORMAT: &str = "INVALID_FORMAT";
/// A request payload exceeds its size limit.
pub const PAYLOAD_TOO_LARGE: &str = "PAYLOAD_TOO_LARGE";
/// A request value is outside its allowed range.
pub const OUT_OF_RANGE: &str = "OUT_OF_RANGE";
/// Storage, the queue or authorization is temporarily unavailable.
pub const UNAVAILABLE: &str = "UNAVAILABLE";
/// A broken invariant or undecodable saved data.
pub const INTERNAL: &str = "INTERNAL";
