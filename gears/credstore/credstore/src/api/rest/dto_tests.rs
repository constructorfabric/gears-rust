//! Unit tests for the credstore REST DTOs.

use credstore_sdk::{SecretType, SecretValue, TenantId};
use uuid::Uuid;

use super::*;

#[test]
fn get_secret_response_dto_debug_redacts_value() {
    let dto = GetSecretResponseDto {
        value: "super-secret-value".to_owned(),
        metadata: SecretMetadataDto {
            owner_tenant_id: Uuid::nil(),
            sharing: SharingModeDto::Tenant,
            is_inherited: false,
            version: 1,
            secret_type: "generic".to_owned(),
            expires_at: None,
        },
    };
    let debug = format!("{dto:?}");
    assert!(
        debug.contains("[REDACTED]"),
        "debug must contain [REDACTED]"
    );
    assert!(
        !debug.contains("super-secret-value"),
        "debug must not contain the plaintext value"
    );
}

#[test]
fn get_secret_response_maps_to_documented_rest_shape() {
    let resp = GetSecretResponse {
        value: SecretValue::from("my-value"),
        id: Uuid::new_v4(),
        owner_tenant_id: TenantId(Uuid::nil()),
        sharing: SharingMode::Shared,
        is_inherited: true,
        version: 7,
        secret_type: SecretType::generic().gts_id().to_owned(),
        expires_at: None,
    };
    let dto = GetSecretResponseDto::try_from_response(&resp).expect("utf-8 value");
    assert_eq!(dto.value, "my-value");
    assert_eq!(dto.metadata.owner_tenant_id, Uuid::nil());
    assert_eq!(dto.metadata.sharing, SharingModeDto::Shared);
    assert!(dto.metadata.is_inherited);
    assert_eq!(dto.metadata.version, 7);
}

#[test]
fn try_from_response_rejects_non_utf8_value() {
    // A binary value (written via the SDK) must not be silently corrupted by
    // lossy decoding — it is rejected with a typed error instead.
    let resp = GetSecretResponse {
        value: SecretValue::new(vec![0xff, 0xfe, 0x00]),
        id: Uuid::new_v4(),
        owner_tenant_id: TenantId(Uuid::nil()),
        sharing: SharingMode::Tenant,
        is_inherited: false,
        version: 1,
        secret_type: SecretType::generic().gts_id().to_owned(),
        expires_at: None,
    };
    let err = GetSecretResponseDto::try_from_response(&resp)
        .expect_err("non-UTF-8 value must be rejected, not lossily decoded");
    assert!(matches!(
        err,
        crate::domain::error::DomainError::Internal { .. }
    ));
}

#[test]
fn sharing_mode_roundtrip() {
    for (dto, sdk) in [
        (SharingModeDto::Private, SharingMode::Private),
        (SharingModeDto::Tenant, SharingMode::Tenant),
        (SharingModeDto::Shared, SharingMode::Shared),
    ] {
        assert_eq!(SharingModeDto::from(sdk), dto);
        assert_eq!(SharingMode::from(dto), sdk);
    }
}

#[test]
fn create_request_debug_redacts_value() {
    let dto = CreateSecretRequestDto {
        reference: "my-ref".to_owned(),
        value: "super-secret-value".to_owned(),
        sharing: SharingModeDto::default(),
        secret_type: None,
        expires_at: None,
    };
    let debug = format!("{dto:?}");
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("super-secret-value"));
    assert!(debug.contains("my-ref"));
}

#[test]
fn update_request_debug_redacts_value() {
    let dto = UpdateSecretRequestDto {
        value: "another-secret".to_owned(),
        sharing: Some(SharingModeDto::default()),
        secret_type: None,
        expires_at: None,
    };
    let debug = format!("{dto:?}");
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("another-secret"));
}
