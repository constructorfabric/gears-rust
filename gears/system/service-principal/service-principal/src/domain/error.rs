//! Domain error model for the service-principal REST facade.
//!
//! Fail-closed. Each variant carries the typed data its `#[error(...)]` message
//! needs; the human-readable message is never assembled at the call site. Mapped
//! to a canonical `Problem` at the REST boundary in `api::rest::error`.

use service_principal_sdk::ServicePrincipalFailure;
use thiserror::Error;
use toolkit_macros::domain_model;

/// Errors raised by the service-principal domain layer.
#[domain_model]
#[derive(Debug, Error)]
pub enum DomainError {
    /// The SPI rejected the input with no state retained (bad name, scope not in
    /// allowlist, quota exceeded, client id taken) → `400`.
    #[error("invalid input: {detail}")]
    InvalidInput {
        /// Human-readable detail of the invalid-input rejection.
        detail: String,
        /// The offending field, when the SPI attributes one.
        field: Option<String>,
    },

    /// The addressed principal does not exist within the tenant → `404`.
    /// (revoke treats this as success-equivalent before conversion — see `service`.)
    #[error("service principal not found")]
    NotFound,

    /// The PDP denied the request (or its constraints failed to compile) → `403`.
    #[error("access denied")]
    AccessDenied,

    /// No SPI provider is registered in the `ClientHub` → `503`.
    #[error("service-principal provider unavailable")]
    ProviderUnavailable,

    /// A clean upstream failure or a PDP evaluation failure — no state retained,
    /// retry is harmless → `503`.
    #[error("upstream unavailable: {detail}")]
    Upstream {
        /// Human-readable detail of the upstream failure.
        detail: String,
    },

    /// Transport uncertainty — the vendor may have retained state → `409`.
    /// A naive retry would hit `InvalidInput` ("name taken"); recovery for a
    /// create is revoke + create, which `409` signals over `503`'s retry-same.
    #[error("upstream outcome ambiguous: {detail}")]
    Ambiguous {
        /// Human-readable detail of the ambiguous outcome.
        detail: String,
    },
}

/// General SPI-failure → domain mapping. NOTE: `revoke` handles `NotFound` as
/// success *before* calling this (idempotent delete), so this blanket mapping is
/// only reached for the non-idempotent operations.
impl From<ServicePrincipalFailure> for DomainError {
    fn from(err: ServicePrincipalFailure) -> Self {
        match err {
            ServicePrincipalFailure::InvalidInput { detail, field } => {
                Self::InvalidInput { detail, field }
            }
            ServicePrincipalFailure::NotFound { .. } => Self::NotFound,
            ServicePrincipalFailure::CleanFailure { detail } => Self::Upstream { detail },
            ServicePrincipalFailure::Ambiguous { detail } => Self::Ambiguous { detail },
        }
    }
}

#[cfg(test)]
mod tests {
    use service_principal_sdk::ServicePrincipalFailure as F;

    use super::*;

    #[test]
    fn maps_sdk_failures_to_domain() {
        assert!(matches!(
            DomainError::from(F::InvalidInput {
                detail: "bad".into(),
                field: Some("name".into())
            }),
            DomainError::InvalidInput { field: Some(_), .. }
        ));
        assert!(matches!(
            DomainError::from(F::NotFound { detail: "x".into() }),
            DomainError::NotFound
        ));
        assert!(matches!(
            DomainError::from(F::CleanFailure { detail: "x".into() }),
            DomainError::Upstream { .. }
        ));
        assert!(matches!(
            DomainError::from(F::Ambiguous { detail: "x".into() }),
            DomainError::Ambiguous { .. }
        ));
    }
}
