// Created: 2026-04-07 by Constructor Tech
use secrecy::{ExposeSecret, SecretString};

use super::*;
use crate::config::{S2sCredentialMapping, TokenMapping};
use uuid::Uuid;

fn default_config() -> StaticAuthNPluginConfig {
    StaticAuthNPluginConfig::default()
}

#[test]
fn accept_all_mode_returns_default_identity() {
    let service = Service::from_config(&default_config());

    let result = service.authenticate("any-token-value");
    assert!(result.is_some());

    let auth = result.unwrap();
    let ctx = &auth.security_context;
    assert_eq!(
        ctx.subject_id(),
        toolkit_security::constants::DEFAULT_SUBJECT_ID
    );
    assert_eq!(
        ctx.subject_tenant_id(),
        toolkit_security::constants::DEFAULT_TENANT_ID
    );
    assert_eq!(ctx.token_scopes(), &["*"]);
    assert_eq!(
        ctx.bearer_token().map(ExposeSecret::expose_secret),
        Some("any-token-value"),
    );
}

#[test]
fn accept_all_mode_rejects_empty_token() {
    let service = Service::from_config(&default_config());

    let result = service.authenticate("");
    assert!(result.is_none());
}

#[test]
fn static_tokens_mode_returns_mapped_identity() {
    let user_a_id = Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
    let tenant_a = Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap();

    let cfg = StaticAuthNPluginConfig {
        mode: AuthNMode::StaticTokens,
        tokens: vec![TokenMapping {
            token: SecretString::from("token-user-a"),
            identity: IdentityConfig {
                subject_id: user_a_id,
                subject_tenant_id: tenant_a,
                token_scopes: vec!["read:data".to_owned()],
                subject_type: None,
            },
        }],
        ..default_config()
    };

    // Redaction proof: the raw token must not appear in Debug output.
    let mapping_debug = format!("{:?}", cfg.tokens[0]);
    assert!(!mapping_debug.contains("token-user-a"));

    let service = Service::from_config(&cfg);

    let result = service.authenticate("token-user-a");
    assert!(result.is_some());

    let auth = result.unwrap();
    let ctx = &auth.security_context;
    assert_eq!(ctx.subject_id(), user_a_id);
    assert_eq!(ctx.subject_tenant_id(), tenant_a);
    assert_eq!(ctx.token_scopes(), &["read:data"]);
    assert_eq!(
        ctx.bearer_token().map(ExposeSecret::expose_secret),
        Some("token-user-a"),
    );
}

#[test]
fn static_tokens_mode_rejects_unknown_token() {
    let cfg = StaticAuthNPluginConfig {
        mode: AuthNMode::StaticTokens,
        tokens: vec![TokenMapping {
            token: SecretString::from("known-token"),
            identity: IdentityConfig::default(),
        }],
        ..default_config()
    };

    let service = Service::from_config(&cfg);

    let result = service.authenticate("unknown-token");
    assert!(result.is_none());
}

#[test]
fn static_tokens_mode_rejects_empty_token() {
    let cfg = StaticAuthNPluginConfig {
        mode: AuthNMode::StaticTokens,
        tokens: vec![],
        ..default_config()
    };

    let service = Service::from_config(&cfg);

    let result = service.authenticate("");
    assert!(result.is_none());
}

#[test]
fn subject_type_propagated_in_security_context() {
    let cfg = StaticAuthNPluginConfig {
        default_identity: IdentityConfig {
            subject_type: Some("user".to_owned()),
            ..IdentityConfig::default()
        },
        ..default_config()
    };

    let service = Service::from_config(&cfg);
    let result = service.authenticate("any-token").unwrap();
    assert_eq!(result.security_context.subject_type(), Some("user"));
}

#[test]
fn subject_type_none_when_not_configured() {
    let service = Service::from_config(&default_config());
    let result = service.authenticate("any-token").unwrap();
    assert_eq!(result.security_context.subject_type(), None);
}

fn s2s_config() -> StaticAuthNPluginConfig {
    let svc_id = Uuid::parse_str("cccccccc-cccc-cccc-cccc-cccccccccccc").unwrap();
    let svc_tenant = Uuid::parse_str("dddddddd-dddd-dddd-dddd-dddddddddddd").unwrap();

    StaticAuthNPluginConfig {
        s2s_credentials: vec![S2sCredentialMapping {
            client_id: "my-service".to_owned(),
            client_secret: SecretString::from("my-secret"),
            identity: IdentityConfig {
                subject_id: svc_id,
                subject_tenant_id: svc_tenant,
                token_scopes: vec!["platform.internal".to_owned()],
                subject_type: Some("service".to_owned()),
            },
            bearer_token: None,
        }],
        ..default_config()
    }
}

#[test]
fn test_exchange_s2s_with_bearer_token() {
    let svc_id = Uuid::parse_str("cccccccc-cccc-cccc-cccc-cccccccccccc").unwrap();
    let svc_tenant = Uuid::parse_str("dddddddd-dddd-dddd-dddd-dddddddddddd").unwrap();

    let cfg = StaticAuthNPluginConfig {
        s2s_credentials: vec![S2sCredentialMapping {
            client_id: "my-service".to_owned(),
            client_secret: SecretString::from("my-secret"),
            identity: IdentityConfig {
                subject_id: svc_id,
                subject_tenant_id: svc_tenant,
                token_scopes: vec!["platform.internal".to_owned()],
                subject_type: Some("service".to_owned()),
            },
            bearer_token: Some(SecretString::from("s2s-issued-token")),
        }],
        ..default_config()
    };

    let service = Service::from_config(&cfg);

    let request = ClientCredentialsRequest {
        client_id: "my-service".to_owned(),
        client_secret: SecretString::from("my-secret"),
        scopes: vec![],
    };

    let result = service.exchange_client_credentials(&request);
    assert!(result.is_some());

    let auth = result.unwrap();
    let ctx = &auth.security_context;
    assert_eq!(ctx.subject_id(), svc_id);
    assert_eq!(ctx.subject_tenant_id(), svc_tenant);
    assert_eq!(ctx.token_scopes(), &["platform.internal"]);
    assert_eq!(ctx.subject_type(), Some("service"));
    assert_eq!(
        ctx.bearer_token().map(ExposeSecret::expose_secret),
        Some("s2s-issued-token"),
    );
}

#[test]
fn s2s_exchange_returns_identity_for_valid_credentials() {
    let service = Service::from_config(&s2s_config());

    let request = ClientCredentialsRequest {
        client_id: "my-service".to_owned(),
        client_secret: SecretString::from("my-secret"),
        scopes: vec![],
    };

    let result = service.exchange_client_credentials(&request);
    assert!(result.is_some());

    let auth = result.unwrap();
    let ctx = &auth.security_context;
    assert_eq!(
        ctx.subject_id(),
        Uuid::parse_str("cccccccc-cccc-cccc-cccc-cccccccccccc").unwrap()
    );
    assert_eq!(
        ctx.subject_tenant_id(),
        Uuid::parse_str("dddddddd-dddd-dddd-dddd-dddddddddddd").unwrap()
    );
    assert_eq!(ctx.token_scopes(), &["platform.internal"]);
    assert_eq!(ctx.subject_type(), Some("service"));
    // bearer_token is None in this config entry — no token propagated to SecurityContext
    assert!(ctx.bearer_token().is_none());
}

#[test]
fn s2s_exchange_rejects_wrong_secret() {
    let service = Service::from_config(&s2s_config());

    let request = ClientCredentialsRequest {
        client_id: "my-service".to_owned(),
        client_secret: SecretString::from("wrong-secret"),
        scopes: vec![],
    };

    let result = service.exchange_client_credentials(&request);
    assert!(result.is_none());
}

#[test]
fn s2s_exchange_rejects_unknown_client_id() {
    let service = Service::from_config(&s2s_config());

    let request = ClientCredentialsRequest {
        client_id: "unknown-service".to_owned(),
        client_secret: SecretString::from("my-secret"),
        scopes: vec![],
    };

    let result = service.exchange_client_credentials(&request);
    assert!(result.is_none());
}

#[test]
fn s2s_exchange_returns_none_with_no_credentials_configured() {
    let service = Service::from_config(&default_config());

    let request = ClientCredentialsRequest {
        client_id: "any-service".to_owned(),
        client_secret: SecretString::from("any-secret"),
        scopes: vec![],
    };

    let result = service.exchange_client_credentials(&request);
    assert!(result.is_none());
}
