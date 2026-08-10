use super::*;
use uuid::uuid;

#[test]
fn shared_round_trip() {
    let original = TenantIdpMetadataV1 {
        version: "v1".into(),
        realm_name: "platform".into(),
        realm_binding: RealmBinding::Shared,
        tenant_group_id: uuid!("9c4a6b2e-0000-0000-0000-000000000001"),
        admin_secret_ref: None,
        admin_client_id: "keycloak-idp-plugin-realm-admin".into(),
    };
    let encoded = MetadataCodec.encode(&original);
    let decoded = MetadataCodec
        .decode(Some(encoded))
        .expect("decodes")
        .expect("Some");
    assert_eq!(decoded, original);
}

#[test]
fn created_round_trip_with_secret_ref() {
    let original = TenantIdpMetadataV1 {
        version: "v1".into(),
        realm_name: "partner-acme".into(),
        realm_binding: RealmBinding::Created,
        tenant_group_id: uuid!("9c4a6b2e-0000-0000-0000-000000000002"),
        admin_secret_ref: Some("keycloak-idp-realm-admin-partner-acme-secret".into()),
        admin_client_id: "keycloak-idp-plugin-realm-admin".into(),
    };
    let encoded = MetadataCodec.encode(&original);
    let decoded = MetadataCodec.decode(Some(encoded)).unwrap().unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn realm_binding_wire_shape_is_snake_case() {
    let v = TenantIdpMetadataV1 {
        version: "v1".into(),
        realm_name: "r".into(),
        realm_binding: RealmBinding::Adopted,
        tenant_group_id: uuid!("9c4a6b2e-0000-0000-0000-000000000003"),
        admin_secret_ref: Some("ref".into()),
        admin_client_id: "client".into(),
    };
    let encoded = MetadataCodec.encode(&v);
    assert_eq!(encoded["realm_binding"], "adopted");
}

#[test]
fn unsupported_version_rejected() {
    let json = serde_json::json!({
        "version": "v99",
        "realm_name": "x",
        "realm_binding": "shared",
        "tenant_group_id": "9c4a6b2e-0000-0000-0000-000000000001",
        "admin_client_id": "y"
    });
    let err = MetadataCodec.decode(Some(json)).unwrap_err();
    assert!(
        matches!(err, DecodeError::UnsupportedVersion { .. }),
        "expected UnsupportedVersion, got {err:?}"
    );
}

#[test]
fn missing_version_rejected() {
    let json = serde_json::json!({"realm_name": "x"});
    let err = MetadataCodec.decode(Some(json)).unwrap_err();
    assert!(
        matches!(err, DecodeError::MissingVersion),
        "expected MissingVersion, got {err:?}"
    );
}

#[test]
fn malformed_blob_rejected() {
    let json = serde_json::json!({
        "version": "v1",
        "realm_name": "x",
        // missing realm_binding, tenant_group_id, admin_client_id
    });
    let err = MetadataCodec.decode(Some(json)).unwrap_err();
    assert!(
        matches!(err, DecodeError::Malformed { .. }),
        "expected Malformed, got {err:?}"
    );
}

#[test]
fn none_returns_none() {
    assert!(MetadataCodec.decode(None).unwrap().is_none());
}

#[test]
fn is_v1_or_compatible_detects_v1() {
    let v1 = serde_json::json!({"version": "v1"});
    let v99 = serde_json::json!({"version": "v99"});
    let no_version = serde_json::json!({});
    assert!(MetadataCodec.is_v1_or_compatible(&v1));
    assert!(!MetadataCodec.is_v1_or_compatible(&v99));
    assert!(!MetadataCodec.is_v1_or_compatible(&no_version));
}

#[test]
fn shared_emits_admin_secret_ref_as_null() {
    let v = TenantIdpMetadataV1 {
        version: "v1".into(),
        realm_name: "platform".into(),
        realm_binding: RealmBinding::Shared,
        tenant_group_id: uuid!("9c4a6b2e-0000-0000-0000-000000000001"),
        admin_secret_ref: None,
        admin_client_id: "keycloak-idp-plugin-realm-admin".into(),
    };
    let encoded = MetadataCodec.encode(&v);
    assert!(
        encoded.get("admin_secret_ref").is_some(),
        "admin_secret_ref must be present (as null) per DESIGN §5.3"
    );
    assert!(
        encoded["admin_secret_ref"].is_null(),
        "admin_secret_ref must be null for Shared binding, got {encoded:?}"
    );
}

#[test]
fn realm_binding_as_metric_label_stable() {
    use crate::domain::metadata_codec::RealmBinding;
    assert_eq!(RealmBinding::Shared.as_metric_label(), "shared");
    assert_eq!(RealmBinding::Adopted.as_metric_label(), "adopted");
    assert_eq!(RealmBinding::Created.as_metric_label(), "created");
}
