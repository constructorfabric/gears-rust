//! Domain errors for the credstore module.

use credstore_sdk::CredStoreError;
use modkit_macros::domain_model;

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

impl From<types_registry_sdk::TypesRegistryError> for DomainError {
    fn from(e: types_registry_sdk::TypesRegistryError) -> Self {
        Self::Internal(e.to_string())
    }
}

impl From<modkit::client_hub::ClientHubError> for DomainError {
    fn from(e: modkit::client_hub::ClientHubError) -> Self {
        Self::Internal(e.to_string())
    }
}

impl From<serde_json::Error> for DomainError {
    fn from(e: serde_json::Error) -> Self {
        Self::Internal(e.to_string())
    }
}

impl From<modkit::plugins::ChoosePluginError> for DomainError {
    fn from(e: modkit::plugins::ChoosePluginError) -> Self {
        match e {
            modkit::plugins::ChoosePluginError::InvalidPluginInstance { gts_id, reason } => {
                Self::InvalidPluginInstance { gts_id, reason }
            }
            modkit::plugins::ChoosePluginError::PluginNotFound { vendor, .. } => {
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
mod tests {
    use modkit::plugins::ChoosePluginError;

    use super::*;

    // ── From<TypesRegistryError> ─────────────────────────────────────────────

    #[test]
    fn from_types_registry_error_becomes_internal() {
        let src = types_registry_sdk::TypesRegistryError::internal("oops");
        let dst = DomainError::from(src);
        assert!(matches!(dst, DomainError::Internal(_)));
    }

    // ── From<ClientHubError> ─────────────────────────────────────────────────

    #[test]
    fn from_client_hub_error_becomes_internal() {
        // Trigger a real ClientHubError by requesting an unregistered type.
        let hub = modkit::client_hub::ClientHub::default();
        let src = hub
            .get::<dyn types_registry_sdk::TypesRegistryClient>()
            .err()
            .unwrap();
        let dst = DomainError::from(src);
        assert!(matches!(dst, DomainError::Internal(_)));
    }

    // ── From<serde_json::Error> ──────────────────────────────────────────────

    #[test]
    fn from_serde_json_error_becomes_internal() {
        let src: serde_json::Error = serde_json::from_str::<i32>("not-json").unwrap_err();
        let dst = DomainError::from(src);
        assert!(matches!(dst, DomainError::Internal(_)));
    }

    // ── From<ChoosePluginError> ──────────────────────────────────────────────

    #[test]
    fn from_choose_plugin_error_not_found_becomes_plugin_not_found() {
        let src = ChoosePluginError::PluginNotFound {
            schema_id: "gts.x.core.test.plugin.v1~".into(),
            vendor: "acme".into(),
        };
        let dst = DomainError::from(src);
        assert!(matches!(dst, DomainError::PluginNotFound { vendor } if vendor == "acme"));
    }

    #[test]
    fn from_choose_plugin_error_invalid_instance_becomes_invalid_plugin_instance() {
        let src = ChoosePluginError::InvalidPluginInstance {
            gts_id: "gts.x.core.test.error.v1~".into(),
            reason: "bad content".into(),
        };
        let dst = DomainError::from(src);
        assert!(
            matches!(dst, DomainError::InvalidPluginInstance { gts_id, reason }
                if gts_id == "gts.x.core.test.error.v1~" && reason == "bad content")
        );
    }

    // ── From<CredStoreError> for DomainError ─────────────────────────────────

    #[test]
    fn from_credstore_error_not_found_becomes_not_found() {
        let dst = DomainError::from(CredStoreError::NotFound);
        assert!(matches!(dst, DomainError::NotFound));
    }

    #[test]
    fn from_credstore_error_no_plugin_available_becomes_plugin_not_found() {
        let dst = DomainError::from(CredStoreError::NoPluginAvailable);
        assert!(matches!(dst, DomainError::PluginNotFound { vendor } if vendor == "unknown"));
    }

    #[test]
    fn from_credstore_error_service_unavailable_becomes_plugin_unavailable() {
        let dst = DomainError::from(CredStoreError::ServiceUnavailable("down".into()));
        assert!(
            matches!(dst, DomainError::PluginUnavailable { gts_id, reason }
            if gts_id == "unknown" && reason == "down")
        );
    }

    #[test]
    fn from_credstore_error_invalid_secret_ref_becomes_internal() {
        let dst = DomainError::from(CredStoreError::InvalidSecretRef {
            reason: "bad".into(),
        });
        assert!(matches!(dst, DomainError::Internal(msg) if msg == "bad"));
    }

    #[test]
    fn from_credstore_error_internal_becomes_internal() {
        let dst = DomainError::from(CredStoreError::Internal("boom".into()));
        assert!(matches!(dst, DomainError::Internal(msg) if msg == "boom"));
    }

    // ── From<DomainError> for CredStoreError ────────────────────────────────

    #[test]
    fn domain_plugin_not_found_becomes_no_plugin_available() {
        let src = DomainError::PluginNotFound {
            vendor: "acme".into(),
        };
        let dst = CredStoreError::from(src);
        assert!(matches!(dst, CredStoreError::NoPluginAvailable));
    }

    #[test]
    fn domain_invalid_plugin_instance_becomes_internal() {
        let src = DomainError::InvalidPluginInstance {
            gts_id: "gts.x.core.test.error.v1~".into(),
            reason: "bad".into(),
        };
        let dst = CredStoreError::from(src);
        assert!(
            matches!(dst, CredStoreError::Internal(ref msg)
                if msg.contains("gts.x.core.test.error.v1~") && msg.contains("bad")),
            "expected Internal with gts_id and reason, got: {dst:?}"
        );
    }

    #[test]
    fn domain_plugin_unavailable_becomes_service_unavailable() {
        let src = DomainError::PluginUnavailable {
            gts_id: "gts.x.core.test.error.v1~".into(),
            reason: "not ready".into(),
        };
        let dst = CredStoreError::from(src);
        assert!(
            matches!(dst, CredStoreError::ServiceUnavailable(ref msg)
                if msg.contains("gts.x.core.test.error.v1~") && msg.contains("not ready")),
            "expected ServiceUnavailable with gts_id and reason, got: {dst:?}"
        );
    }

    #[test]
    fn domain_not_found_becomes_not_found() {
        let dst = CredStoreError::from(DomainError::NotFound);
        assert!(matches!(dst, CredStoreError::NotFound));
    }

    #[test]
    fn domain_types_registry_unavailable_becomes_internal() {
        let src = DomainError::TypesRegistryUnavailable("gone".into());
        let dst = CredStoreError::from(src);
        assert!(matches!(dst, CredStoreError::Internal(msg) if msg == "gone"));
    }

    #[test]
    fn domain_internal_becomes_internal() {
        let src = DomainError::Internal("err".into());
        let dst = CredStoreError::from(src);
        assert!(matches!(dst, CredStoreError::Internal(msg) if msg == "err"));
    }
}
