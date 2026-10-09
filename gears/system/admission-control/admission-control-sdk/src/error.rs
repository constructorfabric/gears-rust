//! The admission-control error family.
//!
//! The public API returns [`AdmissionError`], the platform's canonical error.
//! **Any `Err` from the admission client is a refusal.** Decided refusals
//! travel as `Ok(Verdict::Refused(..))`;
//! [`RefusalCause::to_canonical_error`](crate::models::RefusalCause::to_canonical_error)
//! projects one onto a canonical error.

use toolkit_canonical_errors::resource_error;

/// Error type of the admission client: the platform's canonical error.
pub type AdmissionError = toolkit_canonical_errors::CanonicalError;

/// Resource-error marker for the gear's canonical error family (equal to
/// [`ADMISSION_CONTROL_RESOURCE`](crate::gts::ADMISSION_CONTROL_RESOURCE); the
/// macro takes a literal only, and a unit test holds the two together).
#[resource_error(gts_id!("cf.core.admission_control.admission.v1~"))]
pub struct AdmissionResourceError;

/// Stable reason codes: the four refusal causes and the malformed-call reason.
pub mod reason {
    /// Refused by tenant policy.
    pub const POLICY_REFUSED: &str = "POLICY_REFUSED";
    /// Refused because the request exceeded a size bound.
    pub const REQUEST_TOO_LARGE: &str = "REQUEST_TOO_LARGE";
    /// Refused because the engine judged the operation's properties invalid.
    pub const INVALID_REQUEST: &str = "INVALID_REQUEST";
    /// Refused because a check could not run (retryable unless the condition
    /// is `internal`).
    pub const COULD_NOT_RUN: &str = "COULD_NOT_RUN";
    /// A malformed call: `enforcing_gear`, `action` or `resource_type` failed
    /// identifier validation.
    pub const INVALID_IDENTIFIER: &str = "INVALID_IDENTIFIER";
}

/// A malformed admission call as `invalid_argument` naming the offending field. The detail must be safe for
/// the caller: no property values, no credentials.
#[must_use]
pub fn invalid_request(
    field: impl Into<String>,
    description: impl Into<String>,
    reason_code: impl Into<String>,
) -> AdmissionError {
    AdmissionResourceError::invalid_argument()
        .with_field_violation(field, description, reason_code)
        .create()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::gts::ADMISSION_CONTROL_RESOURCE;

    #[test]
    fn error_family_matches_resource_constant() {
        let err = invalid_request("action", "bad", reason::INVALID_IDENTIFIER);
        assert_eq!(err.resource_type(), Some(ADMISSION_CONTROL_RESOURCE));
        assert_eq!(err.status_code(), 400);
    }
}
