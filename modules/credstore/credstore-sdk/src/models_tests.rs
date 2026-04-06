use super::*;

#[test]
fn secret_ref_valid() {
    assert!(SecretRef::new("partner-openai-key").is_ok());
    assert!(SecretRef::new("api_key_v2").is_ok());
    assert!(SecretRef::new("ABC123").is_ok());
}

#[test]
fn secret_ref_invalid_chars() {
    assert!(SecretRef::new("my:key").is_err());
    assert!(SecretRef::new("my key").is_err());
    assert!(SecretRef::new("key/path").is_err());
}

#[test]
fn secret_ref_empty() {
    assert!(SecretRef::new("").is_err());
}

#[test]
fn secret_ref_too_long() {
    let long = "a".repeat(256);
    assert!(SecretRef::new(long).is_err());
}

#[test]
fn secret_ref_max_length() {
    let max = "a".repeat(255);
    assert!(SecretRef::new(max).is_ok());
}

#[test]
fn secret_ref_deserialize_validates() {
    let valid: Result<SecretRef, _> = serde_json::from_str("\"valid-key_1\"");
    assert!(valid.is_ok());
    assert_eq!(valid.unwrap().as_ref(), "valid-key_1");

    let with_colon: Result<SecretRef, _> = serde_json::from_str("\"my:evil/key\"");
    assert!(with_colon.is_err());

    let empty: Result<SecretRef, _> = serde_json::from_str("\"\"");
    assert!(empty.is_err());
}

#[test]
fn secret_value_debug_redacted() {
    let val = SecretValue::new(b"super-secret".to_vec());
    assert_eq!(format!("{val:?}"), "[REDACTED]");
}

#[test]
fn secret_value_display_redacted() {
    let val = SecretValue::new(b"super-secret".to_vec());
    assert_eq!(format!("{val}"), "[REDACTED]");
}

#[test]
fn secret_value_as_bytes() {
    let val = SecretValue::from("hello");
    assert_eq!(val.as_bytes(), b"hello");
}

#[test]
fn get_secret_response_debug_redacts_value() {
    let resp = GetSecretResponse {
        value: SecretValue::from("secret"),
        owner_tenant_id: TenantId::nil(),
        sharing: SharingMode::Shared,
        is_inherited: true,
    };
    let debug = format!("{resp:?}");
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("secret"));
    assert!(debug.contains("is_inherited: true"));
}

#[test]
fn secret_metadata_debug_redacts_value() {
    let meta = SecretMetadata {
        value: SecretValue::from("secret"),
        owner_id: OwnerId::nil(),
        sharing: SharingMode::Tenant,
        owner_tenant_id: TenantId::nil(),
    };
    let debug = format!("{meta:?}");
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("secret"));
}

#[test]
fn sharing_mode_serde_roundtrip() {
    for (mode, expected_json) in [
        (SharingMode::Private, "\"private\""),
        (SharingMode::Tenant, "\"tenant\""),
        (SharingMode::Shared, "\"shared\""),
    ] {
        let json = serde_json::to_string(&mode).unwrap();
        assert_eq!(json, expected_json);
        let back: SharingMode = serde_json::from_str(&json).unwrap();
        assert_eq!(back, mode);
    }
}

#[test]
fn secret_ref_serialize_roundtrip() {
    let r = SecretRef::new("round-trip").unwrap();
    let json = serde_json::to_string(&r).unwrap();
    assert_eq!(json, "\"round-trip\"");
    let back: SecretRef = serde_json::from_str(&json).unwrap();
    assert_eq!(back.as_ref(), "round-trip");
}
