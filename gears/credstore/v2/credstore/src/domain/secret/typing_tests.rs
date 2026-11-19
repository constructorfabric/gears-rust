//! Unit tests for secret-type trait enforcement.
//!
//! Traits come from the catalog descriptors via
//! [`credstore_sdk::SecretTypeDescriptor::traits`] — the same shape the
//! registry-driven resolver produces for the seeded built-in schemas.

use credstore_sdk::{SecretType, SecretTypeTraits, SecretValue, SharingMode};
use time::{Duration, OffsetDateTime};

use super::{reasons, validate_write};
use crate::domain::error::DomainError;

fn reason_of(err: &DomainError) -> &'static str {
    match err {
        DomainError::TypeViolation { reason, .. } => reason,
        other => panic!("expected TypeViolation, got {other:?}"),
    }
}

/// `(gts_id, traits)` of a catalog type, as the resolver would return them.
fn resolved(name: &str) -> (&'static str, SecretTypeTraits) {
    let t = SecretType::from_name(name).expect("known type");
    (t.gts_id(), t.descriptor().traits())
}

#[test]
fn generic_allows_everything_including_binary() {
    let (id, traits) = resolved("generic");
    for mode in [
        SharingMode::Private,
        SharingMode::Tenant,
        SharingMode::Shared,
    ] {
        validate_write(id, &traits, mode, &SecretValue::new(vec![0xFF, 0x00]), None)
            .expect("generic ok");
    }
}

#[test]
fn personal_token_rejects_non_private_sharing() {
    let (id, traits) = resolved("personal-token");
    validate_write(
        id,
        &traits,
        SharingMode::Private,
        &SecretValue::from("t"),
        None,
    )
    .expect("private ok");
    for mode in [SharingMode::Tenant, SharingMode::Shared] {
        let err =
            validate_write(id, &traits, mode, &SecretValue::from("t"), None).expect_err("denied");
        assert_eq!(reason_of(&err), reasons::SHARING_NOT_ALLOWED_FOR_TYPE);
    }
}

#[test]
fn size_limit_enforced() {
    let (id, traits) = resolved("connection-string");
    let big = SecretValue::new(vec![b'x'; 4 * 1024 + 1]);
    let err = validate_write(id, &traits, SharingMode::Tenant, &big, None).expect_err("too large");
    assert_eq!(reason_of(&err), reasons::VALUE_TOO_LARGE);
}

#[test]
fn utf8_only_rejects_binary() {
    let (id, traits) = resolved("api-key");
    let err = validate_write(
        id,
        &traits,
        SharingMode::Tenant,
        &SecretValue::new(vec![0xFF, 0xFE]),
        None,
    )
    .expect_err("binary rejected");
    assert_eq!(reason_of(&err), reasons::VALUE_NOT_UTF8);
}

#[test]
fn oauth2_client_schema_validated_without_echoing_value() {
    let (id, traits) = resolved("oauth2-client");

    let ok = SecretValue::from(r#"{"client_id":"cid","client_secret":"s3cr3t"}"#);
    validate_write(id, &traits, SharingMode::Tenant, &ok, None).expect("valid payload");

    // Missing required field.
    let missing = SecretValue::from(r#"{"client_id":"cid"}"#);
    let err = validate_write(id, &traits, SharingMode::Tenant, &missing, None).expect_err("schema");
    assert_eq!(reason_of(&err), reasons::VALUE_SCHEMA_VIOLATION);
    assert!(
        !err.to_string().contains("cid"),
        "schema violation detail must not echo the value: {err}"
    );

    // Not JSON at all.
    let not_json = SecretValue::from("just-a-string");
    let err =
        validate_write(id, &traits, SharingMode::Tenant, &not_json, None).expect_err("not json");
    assert_eq!(reason_of(&err), reasons::VALUE_SCHEMA_VIOLATION);
}

#[test]
fn malformed_value_schema_fails_closed_as_service_unavailable() {
    // A `value_schema` trait that is JSON but not a valid JSON Schema —
    // possible only through a broken registration, so 503, not 400.
    let (id, mut traits) = resolved("generic");
    traits.value_schema = Some(serde_json::json!({"type": 42}));
    let err = validate_write(
        id,
        &traits,
        SharingMode::Tenant,
        &SecretValue::from("{}"),
        None,
    )
    .expect_err("bad schema fails closed");
    assert!(
        matches!(err, DomainError::ServiceUnavailable { .. }),
        "got: {err:?}"
    );
}

#[test]
fn expiry_gated_by_expirable_trait() {
    let future = OffsetDateTime::now_utc() + Duration::hours(1);

    // Non-expirable type rejects expires_at.
    let (api_key_id, api_key) = resolved("api-key");
    let err = validate_write(
        api_key_id,
        &api_key,
        SharingMode::Tenant,
        &SecretValue::from("k"),
        Some(future),
    )
    .expect_err("no expiry for api-key");
    assert_eq!(reason_of(&err), reasons::EXPIRY_NOT_SUPPORTED_FOR_TYPE);

    // Expirable type accepts a future expiry, rejects a past one.
    let (bearer_id, bearer) = resolved("bearer-token");
    validate_write(
        bearer_id,
        &bearer,
        SharingMode::Tenant,
        &SecretValue::from("t"),
        Some(future),
    )
    .expect("future expiry ok");
    let past = OffsetDateTime::now_utc() - Duration::hours(1);
    let err = validate_write(
        bearer_id,
        &bearer,
        SharingMode::Tenant,
        &SecretValue::from("t"),
        Some(past),
    )
    .expect_err("past expiry rejected");
    assert_eq!(reason_of(&err), reasons::EXPIRY_IN_THE_PAST);
}
