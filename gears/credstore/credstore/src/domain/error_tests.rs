// Created: 2026-04-07 by Constructor Tech
use toolkit::plugins::ChoosePluginError;

use super::*;

// ── From<CanonicalError> ─────────────────────────────────────────────────

#[test]
fn from_canonical_error_becomes_internal() {
    // The types-registry trait boundary is now `CanonicalError` (ADR 0005);
    // credstore folds any such error into its opaque `Internal`.
    let src = types_registry_sdk::testing::internal("oops");
    let dst = DomainError::from(src);
    assert!(matches!(dst, DomainError::Internal(_)));
}

// ── From<ClientHubError> ─────────────────────────────────────────────────

#[test]
fn from_client_hub_error_becomes_internal() {
    // Trigger a real ClientHubError by requesting an unregistered type.
    let hub = toolkit::client_hub::ClientHub::default();
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
        type_id: "gts.cf.core.test.plugin.v1~".into(),
        vendor: "acme".into(),
    };
    let dst = DomainError::from(src);
    assert!(matches!(dst, DomainError::PluginNotFound { vendor } if vendor == "acme"));
}

#[test]
fn from_choose_plugin_error_invalid_instance_becomes_invalid_plugin_instance() {
    let src = ChoosePluginError::InvalidPluginInstance {
        gts_id: "gts.cf.core.test.error.v1~".into(),
        reason: "bad content".into(),
    };
    let dst = DomainError::from(src);
    assert!(
        matches!(dst, DomainError::InvalidPluginInstance { gts_id, reason }
            if gts_id == "gts.cf.core.test.error.v1~" && reason == "bad content")
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
        gts_id: "gts.cf.core.test.error.v1~".into(),
        reason: "bad".into(),
    };
    let dst = CredStoreError::from(src);
    assert!(
        matches!(dst, CredStoreError::Internal(ref msg)
            if msg.contains("gts.cf.core.test.error.v1~") && msg.contains("bad")),
        "expected Internal with gts_id and reason, got: {dst:?}"
    );
}

#[test]
fn domain_plugin_unavailable_becomes_service_unavailable() {
    let src = DomainError::PluginUnavailable {
        gts_id: "gts.cf.core.test.error.v1~".into(),
        reason: "not ready".into(),
    };
    let dst = CredStoreError::from(src);
    assert!(
        matches!(dst, CredStoreError::ServiceUnavailable(ref msg)
            if msg.contains("gts.cf.core.test.error.v1~") && msg.contains("not ready")),
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
