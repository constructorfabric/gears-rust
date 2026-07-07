//! `DomainError` → [`CanonicalError`] boundary mapping for the credstore REST layer.

use toolkit_canonical_errors::{CanonicalError, resource_error};

use crate::domain::error::DomainError;

// ---------------------------------------------------------------------------
// Resource marker
// ---------------------------------------------------------------------------

#[resource_error(gts_id!("cf.core.credstore.secret.v1~"))]
pub(crate) struct SecretResource;

// ---------------------------------------------------------------------------
// DomainError → CanonicalError
// ---------------------------------------------------------------------------

impl From<DomainError> for CanonicalError {
    fn from(err: DomainError) -> Self {
        match err {
            DomainError::InvalidSecretRef { detail } => SecretResource::invalid_argument()
                .with_field_violation("reference", detail, "INVALID_SECRET_REF")
                .create(),
            DomainError::NotFound => SecretResource::not_found("secret not found")
                .with_resource("secret")
                .create(),
            DomainError::Conflict => SecretResource::already_exists("secret already exists")
                .with_resource("secret")
                .create(),
            // No 412 in the canonical model; optimistic-lock conflicts are Aborted (409).
            DomainError::VersionConflict => {
                SecretResource::aborted("secret version precondition failed")
                    .with_reason("OPTIMISTIC_LOCK_FAILURE")
                    .create()
            }
            DomainError::InvalidPrecondition { detail } => SecretResource::invalid_argument()
                .with_field_violation("If-Match", detail, "INVALID_IF_MATCH")
                .create(),
            DomainError::TypeViolation {
                field,
                reason,
                detail,
            } => SecretResource::invalid_argument()
                .with_field_violation(field, detail, reason)
                .create(),
            DomainError::UnsupportedTransition { detail } => SecretResource::failed_precondition()
                .with_precondition_violation("sharing", detail, "UNSUPPORTED_TRANSITION")
                .create(),
            DomainError::AccessDenied { .. } => SecretResource::permission_denied()
                .with_reason("ACCESS_DENIED")
                .create(),
            DomainError::ServiceUnavailable {
                detail,
                retry_after,
                ..
            } => {
                let mut builder = CanonicalError::service_unavailable().with_detail(detail);
                if let Some(duration) = retry_after {
                    builder = builder.with_retry_after_seconds(duration.as_secs());
                }
                builder.create()
            }
            DomainError::Internal { diagnostic, .. } => {
                CanonicalError::internal(diagnostic).create()
            }
            #[allow(unreachable_patterns)]
            other => {
                CanonicalError::internal(format!("unmapped DomainError variant: {other}")).create()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use toolkit_canonical_errors::CanonicalError;

    use crate::domain::error::DomainError;

    fn status_of(err: DomainError) -> u16 {
        CanonicalError::from(err).status_code()
    }

    #[test]
    fn every_variant_maps_to_a_client_or_server_error() {
        assert_eq!(status_of(DomainError::NotFound), 404);
        assert_eq!(status_of(DomainError::Conflict), 409);
        assert_eq!(
            status_of(DomainError::InvalidSecretRef {
                detail: "bad".to_owned()
            }),
            400
        );
        // Exact pins (not `>= 400`): the handler docs promise these specific
        // codes, so a regression that mapped them to 500 must fail the suite
        // (review finding #14).
        assert_eq!(
            status_of(DomainError::UnsupportedTransition {
                detail: "no".to_owned()
            }),
            400
        );
        assert_eq!(status_of(DomainError::AccessDenied { cause: None }), 403);
        assert_eq!(status_of(DomainError::internal("boom")), 500);
        // No 412 in the canonical model: optimistic-lock conflicts are Aborted (409).
        assert_eq!(status_of(DomainError::VersionConflict), 409);
        assert_eq!(
            status_of(DomainError::InvalidPrecondition {
                detail: "bad".to_owned()
            }),
            400
        );
    }

    #[test]
    fn service_unavailable_carries_retry_after() {
        assert_eq!(
            status_of(DomainError::ServiceUnavailable {
                detail: "later".to_owned(),
                retry_after: Some(Duration::from_secs(30)),
                cause: None,
            }),
            503
        );
        // Without retry_after the other branch is taken.
        assert_eq!(
            status_of(DomainError::ServiceUnavailable {
                detail: "later".to_owned(),
                retry_after: None,
                cause: None,
            }),
            503
        );
    }

    #[test]
    fn resource_error_string_matches_sdk_constant() {
        // The `#[resource_error(...)]` literal on `SecretResource` must equal the
        // SDK's single source of truth (`credstore_sdk::SECRET_RESOURCE_TYPE`);
        // a divergence trips here at test time, not in production. NotFound goes
        // through the `SecretResource` marker, so the built error carries the type.
        let err = CanonicalError::from(DomainError::NotFound);
        assert_eq!(
            err.resource_type(),
            Some(credstore_sdk::SECRET_RESOURCE_TYPE)
        );
    }
}
