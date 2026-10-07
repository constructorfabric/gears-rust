#![allow(clippy::expect_used, clippy::unwrap_used)]
use super::*;
#[test]
fn prototype_l264_dimension_values_few() {
    assert_eq!(validate("region", &["eu".into()])[0].code, "DIM_VALUES_FEW");
}
#[test]
fn prototype_l265_dimension_key_invalid() {
    assert_eq!(
        validate("Region", &["eu".into(), "us".into()])[0].code,
        "DIM_KEY_INVALID"
    );
}
#[test]
fn matrix_11_dimension_value_invalid() {
    assert!(
        validate("region", &["eu".into(), "US".into()])
            .iter()
            .any(|e| e.code == "DIM_VALUE_INVALID")
    );
}
#[test]
fn matrix_11_dimension_value_duplicate() {
    assert!(
        validate("region", &["eu".into(), "eu".into()])
            .iter()
            .any(|e| e.code == "DIM_VALUE_DUPLICATE")
    );
}
#[test]
fn matrix_11_valid_codes() {
    assert!(validate("region", &["eu-1".into(), "us_2".into()]).is_empty());
}

#[test]
fn prototype_dimension_code_grammar() {
    assert!(validate("region_code", &["1-eu".into(), "us_2".into()]).is_empty());
    assert_eq!(
        validate("region-code", &["eu".into(), "us".into()])[0].code,
        "DIM_KEY_INVALID"
    );
}

#[test]
fn prototype_dimension_trims_and_ignores_blank_values() {
    assert!(validate(" region ", &[" eu ".into(), String::new(), "us".into()]).is_empty());
    assert!(
        validate("region", &["eu".into(), " eu ".into()])
            .iter()
            .any(|e| e.code == "DIM_VALUE_DUPLICATE")
    );
}

#[test]
fn a_key_may_be_declared_with_no_values_but_never_with_one() {
    // Spec decision 4: the registry is seeded with `region`, declared and not yet valued.
    assert!(validate("region", &[]).is_empty());
    assert!(validate("region", &[String::new(), " ".into()]).is_empty());
    assert_eq!(validate("region", &["eu".into()])[0].code, "DIM_VALUES_FEW");
}
