//! Domain errors for the `AuthN` resolver.

use authn_resolver_sdk::AuthNResolverError;
use toolkit_macros::domain_model;

/// Internal domain errors.
#[domain_model]
#[derive(thiserror::Error, Debug)]
pub enum DomainError {
    #[error("types registry is not available: {0}")]
    TypesRegistryUnavailable(String),

    #[error("no plugin instances found for vendor '{vendor}'")]
    PluginNotFound { vendor: String },

    #[error("invalid plugin instance content for '{gts_id}': {reason}")]
    InvalidPluginInstance { gts_id: String, reason: String },

    #[error("plugin not available for '{gts_id}': {reason}")]
    PluginUnavailable { gts_id: String, reason: String },

    #[error("unauthorized: {0}")]
    Unauthorized(String),

    #[error("token acquisition failed: {0}")]
    TokenAcquisitionFailed(String),

    #[error("internal error: {0}")]
    Internal(String),
}

// TODO(DE1302): `DomainError::Internal` only carries a String, so the From
// impls below drop the source error. Extend the variant to hold a boxed
// source (or introduce typed variants) so `.source()` returns the original
// error, then remove this allow.
#[allow(unknown_lints, de1302_error_from_to_string)]
mod error_froms {
    use super::DomainError;

    impl From<toolkit_canonical_errors::CanonicalError> for DomainError {
        fn from(e: toolkit_canonical_errors::CanonicalError) -> Self {
            Self::Internal(e.diagnostic().map_or_else(|| e.to_string(), str::to_owned))
        }
    }

    impl From<toolkit::client_hub::ClientHubError> for DomainError {
        fn from(e: toolkit::client_hub::ClientHubError) -> Self {
            Self::Internal(e.to_string())
        }
    }

    impl From<serde_json::Error> for DomainError {
        fn from(e: serde_json::Error) -> Self {
            Self::Internal(e.to_string())
        }
    }
}

impl From<toolkit::plugins::ChoosePluginError> for DomainError {
    fn from(e: toolkit::plugins::ChoosePluginError) -> Self {
        match e {
            toolkit::plugins::ChoosePluginError::InvalidPluginInstance { gts_id, reason } => {
                Self::InvalidPluginInstance { gts_id, reason }
            }
            toolkit::plugins::ChoosePluginError::PluginNotFound { vendor, .. } => {
                Self::PluginNotFound { vendor }
            }
        }
    }
}

impl From<AuthNResolverError> for DomainError {
    fn from(e: AuthNResolverError) -> Self {
        match e {
            AuthNResolverError::Unauthorized(msg) => Self::Unauthorized(msg),
            AuthNResolverError::NoPluginAvailable => Self::PluginNotFound {
                vendor: "unknown".to_owned(),
            },
            AuthNResolverError::ServiceUnavailable(msg) => Self::PluginUnavailable {
                gts_id: "unknown".to_owned(),
                reason: msg,
            },
            AuthNResolverError::TokenAcquisitionFailed(msg) => Self::TokenAcquisitionFailed(msg),
            AuthNResolverError::Internal(msg) => Self::Internal(msg),
        }
    }
}

impl From<DomainError> for AuthNResolverError {
    fn from(e: DomainError) -> Self {
        match e {
            DomainError::PluginNotFound { .. } => Self::NoPluginAvailable,
            DomainError::InvalidPluginInstance { gts_id, reason } => {
                Self::Internal(format!("invalid plugin instance '{gts_id}': {reason}"))
            }
            DomainError::PluginUnavailable { gts_id, reason } => {
                Self::ServiceUnavailable(format!("plugin not available for '{gts_id}': {reason}"))
            }
            DomainError::Unauthorized(msg) => Self::Unauthorized(msg),
            DomainError::TokenAcquisitionFailed(msg) => Self::TokenAcquisitionFailed(msg),
            DomainError::TypesRegistryUnavailable(reason) | DomainError::Internal(reason) => {
                Self::Internal(reason)
            }
        }
    }
}
