//! Error types for the usage emitter crate.
//!
//! [`UsageEmitterError`] is a type alias over [`CanonicalError`]: emitter operations report
//! failures using the canonical error taxonomy used elsewhere in the platform, so callers can
//! match on `CanonicalError::PermissionDenied`, `CanonicalError::Unauthenticated`, etc., without
//! a crate-specific error enum.

use authz_resolver_sdk::{DenyReason, EnforcerError};
use modkit_canonical_errors::CanonicalError;
use tracing::{debug, error};
use usage_collector_sdk::error::UsageRecordError;

/// Errors returned by the usage emitter pipeline.
///
/// Alias of [`CanonicalError`] — pattern-match on its variants directly.
pub type UsageEmitterError = CanonicalError;

fn permission_denied(reason: &str) -> UsageEmitterError {
    UsageRecordError::permission_denied()
        .with_reason(reason)
        .create()
}

fn map_denied(deny_reason: Option<DenyReason>) -> UsageEmitterError {
    let Some(d) = deny_reason else {
        return permission_denied("AUTHORIZATION_DENIED");
    };
    debug!(
        pdp_error_code = %d.error_code,
        pdp_details = d.details.as_deref().unwrap_or(""),
        "PDP explicit deny",
    );
    permission_denied(&d.error_code)
}

/// Maps a PDP [`EnforcerError`] to a fail-closed [`UsageEmitterError`].
///
/// All enforcer outcomes (denial, compile failure, evaluation failure) map to
/// [`CanonicalError::PermissionDenied`] so that PDP infrastructure issues never
/// leak as availability problems to source modules. When the PDP attached a
/// [`DenyReason`], its `error_code` becomes the canonical `reason` (surfaced to
/// clients via Problem) and `details` are logged for diagnostics; otherwise the
/// reason falls back to `AUTHORIZATION_DENIED`.
#[allow(clippy::needless_pass_by_value)]
pub fn enforcer_error_to_emitter_error(e: EnforcerError) -> UsageEmitterError {
    match e {
        EnforcerError::Denied { deny_reason } => map_denied(deny_reason),
        EnforcerError::CompileFailed(source) => {
            error!(
                pdp_error_variant = "CompileFailed",
                error = %source,
                "PDP constraint compilation failed; access denied (fail-closed)",
            );
            permission_denied("AUTHORIZATION_DENIED")
        }
        EnforcerError::EvaluationFailed(source) => {
            error!(
                pdp_error_variant = "EvaluationFailed",
                error = %source,
                "PDP policy evaluation failed; access denied (fail-closed)",
            );
            permission_denied("AUTHORIZATION_DENIED")
        }
    }
}
