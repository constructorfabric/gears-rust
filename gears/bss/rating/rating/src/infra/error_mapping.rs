//! The single `DomainError` → `CanonicalError` ladder (feature 01 §3.5 step 3). The SDK returns the
//! `CanonicalError`; REST renders the same value as RFC 9457 problem details. Every match here is
//! exhaustive, so a new error variant does not compile until its answer is chosen.

use bss_rating_sdk::errors::{
    EvaluationResource, FinalizationPolicyResource, PlatformFinalizationPolicyResource,
    RerateResource, ResultResource, RetentionHoldResource, SourceLossResource, UsageResource,
    reason,
};
use toolkit_canonical_errors::CanonicalError;

use crate::authz::Resource;
use crate::domain::error::DomainError;

/// The `Retry-After` of a `503`: a PDP or Cluster outage is retried after it.
pub const RETRY_AFTER_SECONDS: u64 = 5;

impl From<DomainError> for CanonicalError {
    fn from(err: DomainError) -> Self {
        // @cpt-begin:cpt-cf-bss-rating-algo-foundation-contract-gates:p2:inst-cg-errors
        match err {
            DomainError::NotFound { what } => UsageResource::not_found(format!("{what} not found"))
                .with_resource(what)
                .create(),
            DomainError::NotQuarantined { entry_id } => UsageResource::failed_precondition()
                .with_precondition_violation(
                    entry_id.to_string(),
                    "the inbox entry is not quarantined",
                    reason::INBOX_ENTRY_NOT_QUARANTINED,
                )
                .create(),
            DomainError::SourceNotApplicable { inbox_source } => {
                UsageResource::failed_precondition()
                    .with_precondition_violation(
                        inbox_source.as_str(),
                        "no feature applies entries of this source yet",
                        reason::INBOX_SOURCE_NOT_APPLICABLE,
                    )
                    .create()
            }
            DomainError::ProducerNotVerified { .. }
            | DomainError::OutsideGrant { .. }
            | DomainError::PermissionDenied(_) => UsageResource::permission_denied()
                .with_reason(reason::PERMISSION_DENIED)
                .create(),
            DomainError::IdempotencyKeyReused { key, operation } => {
                key_reused(operation.resource(), key)
            }
            DomainError::InvalidArgument {
                field,
                detail,
                reason,
            } => UsageResource::invalid_argument()
                .with_field_violation(field, detail, reason)
                .create(),
            DomainError::Concurrent(detail) => UsageResource::aborted(detail)
                .with_reason(reason::CONCURRENT_MODIFICATION)
                .create(),
            DomainError::AuthzUnavailable(..)
            | DomainError::Coordination(..)
            | DomainError::DependencyUnavailable { .. } => dependency_unavailable(&err),
            // Logged where they were found: the detail never reaches the caller.
            DomainError::Database(..) | DomainError::CorruptRow(_) => internal(),
            DomainError::Transaction(_)
            | DomainError::LeaderWorkEnded(..)
            | DomainError::Internal(..) => internal_logged(&err),
        }
        // @cpt-end:cpt-cf-bss-rating-algo-foundation-contract-gates:p2:inst-cg-errors
    }
}

/// `AlreadyExists` on the resource the reused key was sent for, naming the key as the resource;
/// it carries no reason code.
fn key_reused(resource: Resource, key: String) -> CanonicalError {
    const DETAIL: &str = "the Idempotency-Key was used with another request";
    match resource {
        Resource::Usage => UsageResource::already_exists(DETAIL)
            .with_resource(key)
            .create(),
        Resource::Result => ResultResource::already_exists(DETAIL)
            .with_resource(key)
            .create(),
        Resource::Rerate => RerateResource::already_exists(DETAIL)
            .with_resource(key)
            .create(),
        Resource::Evaluation => EvaluationResource::already_exists(DETAIL)
            .with_resource(key)
            .create(),
        Resource::FinalizationPolicy => FinalizationPolicyResource::already_exists(DETAIL)
            .with_resource(key)
            .create(),
        Resource::PlatformFinalizationPolicy => {
            PlatformFinalizationPolicyResource::already_exists(DETAIL)
                .with_resource(key)
                .create()
        }
        Resource::SourceLoss => SourceLossResource::already_exists(DETAIL)
            .with_resource(key)
            .create(),
        Resource::RetentionHold => RetentionHoldResource::already_exists(DETAIL)
            .with_resource(key)
            .create(),
    }
}

/// An unavailable dependency: `503` with `Retry-After`.
fn dependency_unavailable(err: &DomainError) -> CanonicalError {
    let source = std::error::Error::source(err).map(ToString::to_string);
    tracing::warn!(target: "bss_rating.errors", error = %err, ?source, "dependency unavailable");
    CanonicalError::service_unavailable()
        .with_retry_after_seconds(RETRY_AFTER_SECONDS)
        .create()
}

/// A `500` whose detail never reaches the caller, for a failure already logged where it was found.
fn internal() -> CanonicalError {
    CanonicalError::internal("internal failure").create()
}

/// A `500` whose detail never reaches the caller, logged here with its source.
fn internal_logged(err: &DomainError) -> CanonicalError {
    let source = std::error::Error::source(err).map(ToString::to_string);
    tracing::error!(target: "bss_rating.errors", error = %err, ?source, "internal failure");
    internal()
}

#[cfg(test)]
#[path = "error_mapping_tests.rs"]
mod tests;
