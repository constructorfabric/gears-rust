//! Unit tests for [`LicenseResolverConfig`].

use super::LicenseResolverConfig;

#[test]
fn an_empty_config_selects_the_first_party_vendor() {
    let cfg: LicenseResolverConfig = serde_json::from_value(serde_json::json!({})).unwrap();
    assert_eq!(cfg.vendor, "constructorfabric");
}

#[test]
fn unknown_fields_are_rejected() {
    let err = serde_json::from_value::<LicenseResolverConfig>(serde_json::json!({ "vndor": "x" }))
        .unwrap_err();
    assert!(err.to_string().contains("unknown field"), "got: {err}");
}

#[test]
fn principal_subject_types_parse_and_default_to_empty() {
    let empty: LicenseResolverConfig = serde_json::from_value(serde_json::json!({})).unwrap();
    assert!(empty.principal_subject_types.is_empty());

    let subject = toolkit_gts::gts_id!("cf.core.lic.subj.v1~test.lic.gateway.seat.v1~");
    let cfg: LicenseResolverConfig = serde_json::from_value(serde_json::json!({
        "principal_subject_types": { "user": subject }
    }))
    .unwrap();
    assert_eq!(
        cfg.principal_subject_types.get("user").map(AsRef::as_ref),
        Some(subject)
    );
}
