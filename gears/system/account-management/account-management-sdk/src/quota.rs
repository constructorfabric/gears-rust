//! Wire `subject` vocabulary for quota violations under
//! [`CanonicalError::ResourceExhausted`].
//!
//! AM emits two `ResourceExhausted` subjects: the hierarchy-integrity
//! single-flight gate and the tenant service-account
//! quota. They stay plain constants with no typed sub-enum. The impl
//! crate's single `From<DomainError> for CanonicalError` ladder
//! references them; the round-trip tests in [`crate::error`] pin them
//! to their `Problem` JSON path (`context.violations[].subject`).
//!
//! [`CanonicalError::ResourceExhausted`]: toolkit_canonical_errors::CanonicalError::ResourceExhausted

/// `violations[].subject` for the hierarchy-integrity check: a check is
/// already in progress and the single-flight gate is held (HTTP 429).
pub const INTEGRITY_CHECK: &str = "integrity_check";

/// `violations[].subject` for a service-account create refused because
/// the tenant has reached its account limit (HTTP 429), regardless of
/// the admission source. This AM wire token is not a QE metric or Quota ID.
/// Clients must not infer retry timing or recovery steps from the detail.
pub const SERVICE_ACCOUNTS: &str = "service_accounts";
