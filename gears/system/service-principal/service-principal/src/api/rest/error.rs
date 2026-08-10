//! `DomainError` → [`CanonicalError`] boundary mapping for the REST layer.

use toolkit_canonical_errors::{CanonicalError, resource_error};

use crate::domain::error::DomainError;

/// Binds canonical errors to the service-principal resource type. The literal MUST
/// equal the SDK's single source of truth — a divergence trips the unit test below.
#[resource_error(gts_id!("cf.core.service_principal.service_principal.v1~"))]
pub(crate) struct ServicePrincipalResource;

impl From<DomainError> for CanonicalError {
    fn from(err: DomainError) -> Self {
        match err {
            // Maps to 400, not 422. rest-api-design §7 would prefer 422 for a
            // valid-body-but-failed-validation error, but the platform canonical
            // taxonomy (toolkit-canonical-errors) maps InvalidArgument → 400 and
            // has no 422 variant; routing every gear through the one canonical
            // Problem pipeline is worth more than the 400/422 nuance here. (An
            // unknown-field body surfaces as 422 from axum's JSON extractor, which
            // sits upstream of this mapping and is outside our control.)
            DomainError::InvalidInput { detail, field } => {
                ServicePrincipalResource::invalid_argument()
                    .with_field_violation(
                        field.unwrap_or_else(|| "request".to_owned()),
                        detail,
                        "INVALID_INPUT",
                    )
                    .create()
            }
            DomainError::NotFound => {
                ServicePrincipalResource::not_found("service principal not found")
                    .with_resource("service_principal")
                    .create()
            }
            DomainError::AccessDenied => ServicePrincipalResource::permission_denied()
                .with_reason("ACCESS_DENIED")
                .create(),
            DomainError::ProviderUnavailable => CanonicalError::service_unavailable()
                .with_detail("service-principal provider unavailable")
                .create(),
            DomainError::Upstream { detail } => CanonicalError::service_unavailable()
                .with_detail(detail)
                .create(),
            // 409, not 503: an Ambiguous create may have half-applied upstream, so
            // "retry the same request" — what 503 signals — would hit InvalidInput
            // ("name taken"). 409 tells the caller to resolve the conflict via the
            // SPI's revoke + create recovery rather than blindly retry. Aborted
            // preserves the detail and carries a machine-readable reason.
            DomainError::Ambiguous { detail } => ServicePrincipalResource::aborted(format!(
                "upstream outcome uncertain, state may have been retained: {detail}"
            ))
            .with_reason("AMBIGUOUS_OUTCOME")
            .create(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status_of(err: DomainError) -> u16 {
        CanonicalError::from(err).status_code()
    }

    #[test]
    fn variants_map_to_expected_statuses() {
        assert_eq!(
            status_of(DomainError::InvalidInput {
                detail: "b".into(),
                field: Some("name".into())
            }),
            400
        );
        assert_eq!(status_of(DomainError::NotFound), 404);
        assert_eq!(status_of(DomainError::AccessDenied), 403);
        assert_eq!(status_of(DomainError::ProviderUnavailable), 503);
        assert_eq!(status_of(DomainError::Upstream { detail: "x".into() }), 503);
        // Ambiguous → 409 (Aborted), not 503: a half-applied create must not
        // advertise "retry the same request".
        assert_eq!(
            status_of(DomainError::Ambiguous { detail: "x".into() }),
            409
        );
    }

    #[test]
    fn resource_error_string_matches_sdk_constant() {
        // The `#[resource_error(...)]` literal must equal the SDK constant; NotFound
        // flows through the marker, so the built error carries the resource type.
        let err = CanonicalError::from(DomainError::NotFound);
        assert_eq!(
            err.resource_type(),
            Some(service_principal_sdk::SERVICE_PRINCIPAL_RESOURCE_TYPE)
        );
    }
}
