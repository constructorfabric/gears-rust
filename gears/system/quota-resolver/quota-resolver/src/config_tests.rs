use super::QuotaResolverConfig;
use quota_resolver_sdk::{QUOTA_SUBJECT_TYPE_ID, QUOTA_TENANT_SUBJECT_TYPE_ID};
use serde_json::json;

const USER: &str = "gts.cf.core.test.user.v1~";
const USER_SUBJECT: &str = "gts.cf.core.quota.subj.v1~cf.core.test.user_subject.v1~";

fn config(key: &str, value: &str) -> QuotaResolverConfig {
    serde_json::from_value(json!({ "principal_subject_types": { key: value } })).unwrap()
}

#[test]
fn an_empty_config_selects_the_first_party_vendor_and_maps_nothing() {
    let cfg: QuotaResolverConfig = serde_json::from_value(json!({})).unwrap();
    assert_eq!(cfg.vendor, "constructorfabric");
    assert!(cfg.principal_subject_types.is_empty());
    cfg.validate().unwrap();
}

#[test]
fn unknown_fields_are_rejected() {
    let err = serde_json::from_value::<QuotaResolverConfig>(json!({ "vndor": "x" })).unwrap_err();
    assert!(err.to_string().contains("unknown field"), "got: {err}");
}

#[test]
fn a_mapping_to_a_derived_subject_type_is_valid() {
    config(USER, USER_SUBJECT).validate().unwrap();
}

#[test]
fn a_key_that_is_not_a_gts_type_id_is_refused() {
    for key in ["user", "gts.cf.core.test.user.v1~a.b.c.d.v1", ""] {
        let err = config(key, USER_SUBJECT).validate().unwrap_err();
        assert!(err.to_string().contains("key"), "got: {err}");
    }
}

#[test]
fn a_value_that_is_not_a_gts_type_id_is_refused() {
    let err = config(USER, "user_subject").validate().unwrap_err();
    assert!(err.to_string().contains("value"), "got: {err}");
}

#[test]
fn a_value_outside_the_subject_base_is_refused() {
    let err = config(USER, "gts.cf.core.test.other.v1~")
        .validate()
        .unwrap_err();
    assert!(err.to_string().contains("not derived"), "got: {err}");
}

#[test]
fn a_value_that_is_the_base_or_the_tenant_type_is_refused() {
    for value in [QUOTA_SUBJECT_TYPE_ID, QUOTA_TENANT_SUBJECT_TYPE_ID] {
        let err = config(USER, value).validate().unwrap_err();
        assert!(
            err.to_string().contains("tenant subject type"),
            "got: {err}"
        );
    }
}

#[test]
fn a_blank_vendor_is_refused() {
    let cfg: QuotaResolverConfig = serde_json::from_value(json!({ "vendor": " " })).unwrap();
    let err = cfg.validate().unwrap_err();
    assert!(err.to_string().contains("vendor"), "got: {err}");
}
