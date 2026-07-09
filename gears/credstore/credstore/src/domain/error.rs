// Updated: 2026-04-07 by Constructor Tech
//! Domain errors for the credstore gear.

use credstore_sdk::CredStoreError;
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

    #[error("secret not found")]
    NotFound,

    #[error("internal error: {0}")]
    Internal(String),
}

// TODO(DE1302): `DomainError::Internal` only carries a String, so these From
// impls drop the source error. Extend the variant to hold a boxed source (or
// introduce typed variants) so `.source()` returns the original error, then
// remove these allows.
#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<toolkit_canonical_errors::CanonicalError> for DomainError {
    fn from(e: toolkit_canonical_errors::CanonicalError) -> Self {
        // Prefer the in-process diagnostic for `Internal`/`Unknown` (the wire
        // strips it); other categories carry their message in `detail()` via
        // `to_string()`.
        Self::Internal(e.diagnostic().map_or_else(|| e.to_string(), str::to_owned))
    }
}

#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<toolkit::client_hub::ClientHubError> for DomainError {
    fn from(e: toolkit::client_hub::ClientHubError) -> Self {
        Self::Internal(e.to_string())
    }
}

#[allow(unknown_lints, de1302_error_from_to_string)]
impl From<serde_json::Error> for DomainError {
    fn from(e: serde_json::Error) -> Self {
        Self::Internal(e.to_string())
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

impl From<CredStoreError> for DomainError {
    fn from(e: CredStoreError) -> Self {
        match e {
            CredStoreError::NotFound => Self::NotFound,
            // CredStoreError variants don't carry vendor/gts_id, so these
            // fields cannot be populated from the error alone.
            CredStoreError::NoPluginAvailable => Self::PluginNotFound {
                vendor: "unknown".to_owned(),
            },
            CredStoreError::ServiceUnavailable(msg) => Self::PluginUnavailable {
                gts_id: "unknown".to_owned(),
                reason: msg,
            },
            CredStoreError::InvalidSecretRef { reason } => Self::Internal(reason),
            CredStoreError::Internal(msg) => Self::Internal(msg),
        }
    }
}

impl From<DomainError> for CredStoreError {
    fn from(e: DomainError) -> Self {
        match e {
            DomainError::PluginNotFound { .. } => Self::NoPluginAvailable,
            DomainError::InvalidPluginInstance { gts_id, reason } => {
                Self::Internal(format!("invalid plugin instance '{gts_id}': {reason}"))
            }
            DomainError::PluginUnavailable { gts_id, reason } => {
                Self::ServiceUnavailable(format!("plugin not available for '{gts_id}': {reason}"))
            }
            DomainError::NotFound => Self::NotFound,
            DomainError::TypesRegistryUnavailable(reason) | DomainError::Internal(reason) => {
                Self::Internal(reason)
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "error_tests.rs"]
mod error_tests;
