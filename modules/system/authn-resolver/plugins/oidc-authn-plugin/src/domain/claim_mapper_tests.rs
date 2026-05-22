use super::*;
use crate::domain::metrics::{TOKEN_REJECTION_REASON_INVALID_TENANT, test_harness::MetricsHarness};
use crate::test_support::test_fixtures::claims;
use serde_json::json;

#[test]
fn extract_subject_id_returns_uuid_for_valid_sub()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let claims = claims(json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
    }));

    let subject_id = extract_subject_id(&claims)?;
    assert_eq!(
        subject_id,
        Uuid::from_u128(0x550e_8400_e29b_41d4_a716_4466_5544_0000_u128)
    );
    Ok(())
}

#[test]
fn extract_subject_id_rejects_non_uuid_sub() {
    let claims = claims(json!({
        "sub": "not-a-uuid",
    }));

    let err = extract_subject_id(&claims);
    assert!(matches!(
        err,
        Err(AuthNResolverError::Unauthorized(msg)) if msg == "invalid subject id"
    ));
}

#[test]
fn extract_subject_id_rejects_missing_sub() {
    let claims = claims(json!({
        "tenant_id": "550e8400-e29b-41d4-a716-446655440001",
    }));

    let err = extract_subject_id(&claims);
    assert!(matches!(
        err,
        Err(AuthNResolverError::Unauthorized(msg)) if msg == "invalid subject id"
    ));
}

#[test]
fn extract_tenant_id_returns_uuid_when_claim_is_present()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let claims = claims(json!({
        "tenant_id": "550e8400-e29b-41d4-a716-446655440010",
    }));

    let tenant_id = extract_tenant_id(&claims, "tenant_id", &MetricsHarness::new().metrics())?;
    assert_eq!(
        tenant_id,
        Uuid::from_u128(0x550e_8400_e29b_41d4_a716_4466_5544_0010_u128)
    );
    Ok(())
}

#[test]
fn extract_tenant_id_rejects_missing_claim() {
    let claims = claims(json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
    }));

    let err = extract_tenant_id(&claims, "tenant_id", &MetricsHarness::new().metrics());
    assert!(matches!(
        err,
        Err(AuthNResolverError::Unauthorized(msg)) if msg == "missing claim"
    ));
}

#[test]
fn extract_tenant_id_records_invalid_tenant_rejection_metric() {
    let harness = MetricsHarness::new();
    let metrics = harness.metrics();
    let claims = claims(json!({
        "tenant_id": "not-a-uuid",
    }));

    let err = extract_tenant_id(&claims, "tenant_id", &metrics);

    assert!(matches!(
        err,
        Err(AuthNResolverError::Unauthorized(msg)) if msg == "invalid tenant_id"
    ));
    harness.force_flush();
    assert_eq!(
        harness.counter_value(
            crate::domain::metrics::AUTHN_TOKEN_REJECTED_TOTAL,
            &[("reason", TOKEN_REJECTION_REASON_INVALID_TENANT)]
        ),
        1
    );
}

#[test]
fn extract_tenant_id_supports_custom_claim_name()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    let claims = claims(json!({
        "tenant": "550e8400-e29b-41d4-a716-446655440020",
    }));

    let tenant_id = extract_tenant_id(&claims, "tenant", &MetricsHarness::new().metrics())?;
    assert_eq!(
        tenant_id,
        Uuid::from_u128(0x550e_8400_e29b_41d4_a716_4466_5544_0020_u128)
    );
    Ok(())
}

#[test]
fn extract_user_type_returns_value_when_present() {
    let claims = claims(json!({
        "user_type": "human",
    }));
    assert_eq!(extract_user_type(&claims), Some("human".to_owned()));
}

#[test]
fn extract_user_type_returns_none_when_absent() {
    let claims = claims(json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
    }));
    assert_eq!(extract_user_type(&claims), None);
}

#[test]
fn detect_app_type_prefers_azp_for_first_party_match() {
    let claims = claims(json!({
        "azp": "cyber-fabric-portal",
    }));
    let first_party_clients = vec![
        "cyber-fabric-portal".to_owned(),
        "cyber-fabric-cli".to_owned(),
    ];

    let app_type = detect_app_type(&claims, &first_party_clients);
    assert_eq!(app_type, AppType::FirstParty);
}

#[test]
fn detect_app_type_falls_back_to_client_id_when_azp_absent() {
    let claims = claims(json!({
        "client_id": "cyber-fabric-cli",
    }));
    let first_party_clients = vec![
        "cyber-fabric-portal".to_owned(),
        "cyber-fabric-cli".to_owned(),
    ];

    let app_type = detect_app_type(&claims, &first_party_clients);
    assert_eq!(app_type, AppType::FirstParty);
}

#[test]
fn detect_app_type_returns_third_party_when_client_is_unknown() {
    let claims = claims(json!({
        "azp": "partner-app",
    }));
    let first_party_clients = vec![
        "cyber-fabric-portal".to_owned(),
        "cyber-fabric-cli".to_owned(),
    ];

    let app_type = detect_app_type(&claims, &first_party_clients);
    assert_eq!(app_type, AppType::ThirdParty);
}

#[test]
fn extract_scopes_returns_wildcard_for_first_party() {
    let claims = claims(json!({
        "scope": "read:resource write:resource",
    }));

    let scopes = extract_scopes(&claims, AppType::FirstParty);
    assert_eq!(scopes, vec!["*".to_owned()]);
}

#[test]
fn extract_scopes_splits_scope_claim_for_third_party() {
    let claims = claims(json!({
        "scope": "read:resource write:resource",
    }));

    let scopes = extract_scopes(&claims, AppType::ThirdParty);
    assert_eq!(
        scopes,
        vec!["read:resource".to_owned(), "write:resource".to_owned()]
    );
}

#[test]
fn extract_scopes_returns_empty_when_scope_claim_is_missing_for_third_party() {
    let claims = claims(json!({
        "azp": "partner-app",
    }));

    let scopes = extract_scopes(&claims, AppType::ThirdParty);
    assert!(scopes.is_empty());
}

#[test]
fn map_builds_security_context_for_first_party_claims() {
    let claims = claims(json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
        "tenant_id": "550e8400-e29b-41d4-a716-446655440001",
        "user_type": "user",
        "azp": "cyber-fabric-portal",
        "scope": "read:resource write:resource",
    }));

    let config = default_config();
    let options = ClaimMapperOptions {
        first_party_clients: vec![
            "cyber-fabric-portal".to_owned(),
            "cyber-fabric-cli".to_owned(),
        ],
        ..ClaimMapperOptions::default()
    };

    let mapped = map_with_options(&claims, &config, &options, &MetricsHarness::new().metrics());
    assert!(mapped.is_ok());

    let context = mapped.expect("first-party claims should map to security context");
    assert_eq!(
        context.subject_id(),
        Uuid::from_u128(0x550e_8400_e29b_41d4_a716_4466_5544_0000_u128)
    );
    assert_eq!(
        context.subject_tenant_id(),
        Uuid::from_u128(0x550e_8400_e29b_41d4_a716_4466_5544_0001_u128)
    );
    assert_eq!(context.subject_type(), Some("user"));
    assert_eq!(context.token_scopes(), &["*"]); // first-party wildcard
}

#[test]
fn map_builds_security_context_for_third_party_claims() {
    let claims = claims(json!({
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

    let mapped = map_with_options(&claims, &config, &options, &MetricsHarness::new().metrics());
    assert!(mapped.is_ok());

    let context = mapped.expect("third-party claims should map to security context");
    assert_eq!(context.subject_type(), None);
    assert_eq!(
        context.token_scopes(),
        &["read:orders".to_owned(), "write:orders".to_owned()]
    );
}

#[test]
fn map_rejects_invalid_subject_claim() {
    let claims = claims(json!({
        "sub": "not-a-uuid",
        "tenant_id": "550e8400-e29b-41d4-a716-446655440001",
    }));

    let err = map(&claims, &default_config(), &MetricsHarness::new().metrics());
    assert!(matches!(
        err,
        Err(AuthNResolverError::Unauthorized(msg)) if msg == "invalid subject id"
    ));
}

#[test]
fn map_rejects_missing_tenant_claim() {
    let claims = claims(json!({
        "sub": "550e8400-e29b-41d4-a716-446655440000",
    }));

    let err = map(&claims, &default_config(), &MetricsHarness::new().metrics());
    assert!(matches!(
        err,
        Err(AuthNResolverError::Unauthorized(msg)) if msg == "missing claim"
    ));
}

#[test]
fn map_supports_custom_tenant_claim_name() {
    let claims = claims(json!({
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

    let mapped = map_with_options(&claims, &config, &options, &MetricsHarness::new().metrics());
    assert!(mapped.is_ok());

    let context = mapped.expect("custom tenant claim should be honored");
    assert_eq!(
        context.subject_tenant_id(),
        Uuid::from_u128(0x550e_8400_e29b_41d4_a716_4466_5544_1001_u128)
    );
    assert_eq!(context.token_scopes(), &["*"]);
}
