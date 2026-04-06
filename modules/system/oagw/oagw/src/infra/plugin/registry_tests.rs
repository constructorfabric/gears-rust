use std::sync::Arc;

use crate::domain::test_support::MockCredStoreClient;

use super::*;

fn make_registry() -> AuthPluginRegistry {
    AuthPluginRegistry::with_builtins(
        Arc::new(MockCredStoreClient::empty()),
        None,
        TokenCacheConfig::default(),
    )
}

#[test]
fn resolves_apikey_plugin() {
    let registry = make_registry();
    assert!(registry.resolve(APIKEY_AUTH_PLUGIN_ID).is_ok());
}

#[test]
fn resolves_noop_plugin() {
    let registry = make_registry();
    assert!(registry.resolve(NOOP_AUTH_PLUGIN_ID).is_ok());
}

#[test]
fn resolves_oauth2_client_cred_form_plugin() {
    let registry = make_registry();
    assert!(registry.resolve(OAUTH2_CLIENT_CRED_AUTH_PLUGIN_ID).is_ok());
}

#[test]
fn resolves_oauth2_client_cred_basic_plugin() {
    let registry = make_registry();
    assert!(
        registry
            .resolve(OAUTH2_CLIENT_CRED_BASIC_AUTH_PLUGIN_ID)
            .is_ok()
    );
}

#[test]
fn unknown_plugin_returns_error() {
    let registry = make_registry();
    let err = registry.resolve("gts.x.core.oagw.auth_plugin.v1~x.core.oagw.unknown.v1");
    assert!(err.is_err());
}

#[test]
fn resolves_required_headers_guard_plugin() {
    let registry = GuardPluginRegistry::with_builtins();
    assert!(registry.resolve(REQUIRED_HEADERS_GUARD_PLUGIN_ID).is_ok());
}

#[test]
fn unknown_guard_plugin_returns_error() {
    let registry = GuardPluginRegistry::with_builtins();
    let err = registry.resolve("gts.x.core.oagw.guard_plugin.v1~x.core.oagw.unknown.v1");
    assert!(err.is_err());
}

#[test]
fn is_guard_plugin_matches_guard_schema() {
    assert!(GuardPluginRegistry::is_guard_plugin(
        REQUIRED_HEADERS_GUARD_PLUGIN_ID
    ));
    assert!(!GuardPluginRegistry::is_guard_plugin(APIKEY_AUTH_PLUGIN_ID));
}

#[test]
fn resolves_request_id_transform_plugin() {
    let registry = TransformPluginRegistry::with_builtins();
    assert!(registry.resolve(REQUEST_ID_TRANSFORM_PLUGIN_ID).is_ok());
}

#[test]
fn unknown_transform_plugin_returns_error() {
    let registry = TransformPluginRegistry::with_builtins();
    let err = registry.resolve("gts.x.core.oagw.transform_plugin.v1~x.core.oagw.unknown.v1");
    assert!(err.is_err());
}

#[test]
fn is_transform_plugin_matches_transform_schema() {
    assert!(TransformPluginRegistry::is_transform_plugin(
        REQUEST_ID_TRANSFORM_PLUGIN_ID
    ));
    assert!(!TransformPluginRegistry::is_transform_plugin(
        APIKEY_AUTH_PLUGIN_ID
    ));
}
