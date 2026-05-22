#![allow(clippy::expect_used, clippy::panic)]

//! Integration tests for claim-to-security-context mapping.
//!
//! These tests validate the externally visible behavior of `claim_mapper::map`
//! against Story 3 acceptance criteria.

use authn_resolver_sdk::AuthNResolverError;
use oidc_authn_plugin::claim_mapper::{
    self, ClaimMapperConfig, ClaimMapperOptions, default_config,
};
use serde_json::json;
use uuid::Uuid;

pub mod common;

use common::{claims, create_test_metrics};

#[test]
fn map_accepts_first_party_and_assigns_wildcard_scope() {
    let input = claims(json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
        "tenant_id": "550e8400-e29b-41d4-a716-446655440001",
        "user_type": "user",
        "azp": "cyber-fabric-portal",
        "scope": "read write",
    }));

    let config = default_config();
    let options = ClaimMapperOptions {
        first_party_clients: vec![
            "cyber-fabric-portal".to_owned(),
            "cyber-fabric-cli".to_owned(),
        ],
        ..ClaimMapperOptions::default()
    };

    let mapped = claim_mapper::map_with_options(&input, &config, &options, &create_test_metrics());
    match mapped {
        Ok(ctx) => {
            assert_eq!(
                ctx.subject_id(),
                Uuid::from_u128(0x550e_8400_e29b_41d4_a716_4466_5544_0000_u128)
            );
            assert_eq!(
                ctx.subject_tenant_id(),
                Uuid::from_u128(0x550e_8400_e29b_41d4_a716_4466_5544_0001_u128)
            );
            assert_eq!(ctx.subject_type(), Some("user"));
            assert_eq!(ctx.token_scopes(), &["*"]);
        }
        Err(err) => panic!("expected successful mapping, got error: {err}"),
    }
}

#[test]
fn map_assigns_literal_scopes_for_third_party() {
    let input = claims(json!({
        "sub": "550e8400-e29b-41d4-a716-446655440100",
        "tenant_id": "550e8400-e29b-41d4-a716-446655440101",
        "azp": "partner-integration",
        "scope": "read:orders write:orders",
    }));

    let config = default_config();
    let options = ClaimMapperOptions {
        first_party_clients: vec![
            "cyber-fabric-portal".to_owned(),
            "cyber-fabric-cli".to_owned(),
        ],
        ..ClaimMapperOptions::default()
    };

    let mapped = claim_mapper::map_with_options(&input, &config, &options, &create_test_metrics());
    match mapped {
        Ok(ctx) => {
            assert_eq!(ctx.subject_type(), None);
            assert_eq!(
                ctx.token_scopes(),
                &["read:orders".to_owned(), "write:orders".to_owned()]
            );
        }
        Err(err) => panic!("expected successful mapping, got error: {err}"),
    }
}

#[test]
fn map_rejects_invalid_subject_id_with_unauthorized() {
    let input = claims(json!({
        "sub": "not-a-uuid",
        "tenant_id": "550e8400-e29b-41d4-a716-446655440001",
    }));

    let err = claim_mapper::map(&input, &default_config(), &create_test_metrics());
    assert!(matches!(
        err,
        Err(AuthNResolverError::Unauthorized(message)) if message == "invalid subject id"
    ));
}

#[test]
fn map_rejects_missing_tenant_id_with_unauthorized() {
    let input = claims(json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
    }));

    let err = claim_mapper::map(&input, &default_config(), &create_test_metrics());
    assert!(matches!(
        err,
        Err(AuthNResolverError::Unauthorized(message)) if message == "missing claim"
    ));
}

#[test]
fn map_supports_custom_tenant_claim_name() {
    let input = claims(json!({
        "sub": "550e8400-e29b-41d4-a716-446655441000",
        "tenant": "550e8400-e29b-41d4-a716-446655441001",
        "client_id": "cyber-fabric-cli",
    }));

    let config = ClaimMapperConfig {
        subject_tenant_id: "tenant".to_owned(),
        ..default_config()
    };
    let options = ClaimMapperOptions {
        first_party_clients: vec!["cyber-fabric-cli".to_owned()],
        ..ClaimMapperOptions::default()
    };

    let mapped = claim_mapper::map_with_options(&input, &config, &options, &create_test_metrics());
    match mapped {
        Ok(ctx) => {
            assert_eq!(
                ctx.subject_tenant_id(),
                Uuid::from_u128(0x550e_8400_e29b_41d4_a716_4466_5544_1001_u128)
            );
            assert_eq!(ctx.token_scopes(), &["*"]);
        }
        Err(err) => panic!("expected successful mapping, got error: {err}"),
    }
}
