//! The single `DomainError` → `CanonicalError` ladder (feature 01 §3.5 step 3). The SDK returns the
//! `CanonicalError`; REST renders the same value as RFC 9457 problem details.

use bss_rating_sdk::errors::{UsageResource, reason};
use toolkit_canonical_errors::CanonicalError;

use crate::domain::error::DomainError;

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
            DomainError::ProducerNotVerified { .. } | DomainError::OutsideGrant { .. } => {
                UsageResource::permission_denied()
                    .with_reason(reason::PERMISSION_DENIED)
                    .create()
            }
            // AlreadyExists carries no reason code: the resource it names is the key.
            DomainError::IdempotencyKeyReused { key } => {
                UsageResource::already_exists("the Idempotency-Key was used with another request")
                    .with_resource(key)
                    .create()
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
            DomainError::PermissionDenied(_) => UsageResource::permission_denied()
                .with_reason(reason::PERMISSION_DENIED)
                .create(),
            DomainError::AuthzUnavailable(..)
            | DomainError::Coordination(..)
            | DomainError::Transaction(_)
            | DomainError::LeaderWorkEnded(_)
            | DomainError::CorruptRow(_)
            | DomainError::Database(..) => infrastructure_failure(&err),
        }
        // @cpt-end:cpt-cf-bss-rating-algo-foundation-contract-gates:p2:inst-cg-errors
    }
}

/// An unavailable dependency is `503` with `Retry-After`; anything else that is not the caller's
/// is a `500` whose detail never reaches the caller. A storage failure and a corrupt row were
/// logged where they were found, so they are not logged again here.
fn infrastructure_failure(err: &DomainError) -> CanonicalError {
    match err {
        DomainError::AuthzUnavailable(..) | DomainError::Coordination(..) => {
            let source = std::error::Error::source(err).map(ToString::to_string);
            tracing::warn!(target: "bss_rating.errors", error = %err, ?source, "dependency unavailable");
            CanonicalError::service_unavailable()
                .with_retry_after_seconds(5)
                .create()
        }
        DomainError::Database(..) | DomainError::CorruptRow(_) => {
            CanonicalError::internal("internal failure").create()
        }
        _ => {
            let source = std::error::Error::source(err).map(ToString::to_string);
            tracing::error!(target: "bss_rating.errors", error = %err, ?source, "internal failure");
            CanonicalError::internal("internal failure").create()
        }
    }
}

#[cfg(test)]
#[path = "error_mapping_tests.rs"]
mod tests;
