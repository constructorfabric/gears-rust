use secrecy::ExposeSecret;

use super::*;

#[test]
fn test_security_context_builder_full() {
    let subject_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440001").unwrap();
    let subject_tenant_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440002").unwrap();

    let ctx = SecurityContext::builder()
        .subject_id(subject_id)
        .subject_type("user")
        .subject_tenant_id(subject_tenant_id)
        .token_scopes(vec!["read:events".to_owned(), "write:events".to_owned()])
        .bearer_token("test-token-123".to_owned())
        .build()
        .unwrap();

    assert_eq!(ctx.subject_id(), subject_id);
    assert_eq!(ctx.subject_tenant_id(), subject_tenant_id);
    assert_eq!(ctx.token_scopes(), &["read:events", "write:events"]);
    assert_eq!(
        ctx.bearer_token().map(ExposeSecret::expose_secret),
        Some("test-token-123"),
    );
}

#[test]
fn test_security_context_builder_missing_subject_id() {
    let err = SecurityContext::builder()
        .subject_tenant_id(Uuid::parse_str("550e8400-e29b-41d4-a716-446655440002").unwrap())
        .build();

    assert!(matches!(
        err,
        Err(SecurityContextBuildError::MissingSubjectId)
    ));
}

#[test]
fn test_security_context_builder_missing_tenant_id() {
    let err = SecurityContext::builder()
        .subject_id(Uuid::parse_str("550e8400-e29b-41d4-a716-446655440001").unwrap())
        .build();

    assert!(matches!(
        err,
        Err(SecurityContextBuildError::MissingSubjectTenantId)
    ));
}

#[test]
fn test_security_context_builder_missing_both() {
    let err = SecurityContext::builder().build();

    assert!(matches!(
        err,
        Err(SecurityContextBuildError::MissingSubjectId)
    ));
}

#[test]
fn test_security_context_anonymous() {
    let ctx = SecurityContext::anonymous();

    assert_eq!(ctx.subject_id(), Uuid::default());
    assert_eq!(ctx.subject_tenant_id(), Uuid::default());
    assert!(ctx.token_scopes().is_empty());
    assert!(ctx.bearer_token().is_none());
}

#[test]
fn test_security_context_builder_chaining() {
    let subject_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440001").unwrap();
    let subject_tenant_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440002").unwrap();

    let ctx = SecurityContext::builder()
        .subject_id(subject_id)
        .subject_type("user")
        .subject_tenant_id(subject_tenant_id)
        .build()
        .unwrap();

    assert_eq!(ctx.subject_id(), subject_id);
}

#[test]
fn test_security_context_clone() {
    let subject_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440001").unwrap();
    let subject_tenant_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440002").unwrap();

    let ctx1 = SecurityContext::builder()
        .subject_id(subject_id)
        .subject_tenant_id(subject_tenant_id)
        .token_scopes(vec!["*".to_owned()])
        .bearer_token("secret".to_owned())
        .build()
        .unwrap();

    let ctx2 = ctx1.clone();

    assert_eq!(ctx2.subject_id(), ctx1.subject_id());
    assert_eq!(ctx2.subject_tenant_id(), ctx1.subject_tenant_id());
    assert_eq!(ctx2.token_scopes(), ctx1.token_scopes());
    assert_eq!(
        ctx2.bearer_token().map(ExposeSecret::expose_secret),
        ctx1.bearer_token().map(ExposeSecret::expose_secret),
    );
}

#[test]
fn test_security_context_serialize_deserialize() {
    let subject_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440001").unwrap();
    let subject_tenant_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440002").unwrap();

    let original = SecurityContext::builder()
        .subject_id(subject_id)
        .subject_type("user")
        .subject_tenant_id(subject_tenant_id)
        .token_scopes(vec!["admin".to_owned()])
        .bearer_token("secret-token".to_owned())
        .build()
        .unwrap();

    let serialized = serde_json::to_string(&original).unwrap();
    let deserialized: SecurityContext = serde_json::from_str(&serialized).unwrap();

    assert_eq!(deserialized.subject_id(), original.subject_id());
    assert_eq!(
        deserialized.subject_tenant_id(),
        original.subject_tenant_id()
    );
    assert_eq!(deserialized.token_scopes(), original.token_scopes());
    assert!(deserialized.bearer_token().is_none());
}

#[test]
fn test_security_context_bearer_token_not_serialized() {
    let ctx = SecurityContext::anonymous();

    let serialized = serde_json::to_string(&ctx).unwrap();
    assert!(!serialized.contains("bearer_token"));
}

#[test]
fn test_security_context_empty_scopes() {
    let ctx = SecurityContext::anonymous();

    assert!(ctx.token_scopes().is_empty());
}
