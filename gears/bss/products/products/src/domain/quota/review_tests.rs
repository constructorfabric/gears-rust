//! A submitter's proposal (P-D-269, P-D-270): the record's bounds convert exactly into each plan
//! unit or are refused, the qualified profile needs every operation and initial installation, and
//! the bases and units are non-empty sets; the stored proposal reads back as written.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::domain::quota::evidence::Evidence;
use crate::quota_test_support::{
    ALLOCATED_VCPU, BYTES_UNIT, REGIONAL_POOL, VCPU_UNIT, vcpu_record,
};
use serde_json::{Value, json};

fn unit(name: &str, numerator: &str, denominator: &str) -> QuotaUnitV1 {
    let id = format!("gts.cf.bss.products.quota_unit.v1~cf.compute.quota_adapter.{name}.v1");
    serde_json::from_value(json!({
        "id": id,
        "symbol": name,
        "canonical_unit_ref": BYTES_UNIT,
        "numerator": numerator,
        "denominator": denominator,
    }))
    .unwrap()
}

fn record_of(raw: Value) -> QuotaCapabilityV1 {
    serde_json::from_value(raw).unwrap()
}

fn scope() -> QuotaScopePolicyV1 {
    serde_json::from_value(
        json!({"id": REGIONAL_POOL, "subject_selector": "resource_owner",
        "aggregation": "single_verified_scope", "required_dimensions": ["region"]}),
    )
    .unwrap()
}

fn accounting() -> QuotaAccountingPolicyV1 {
    serde_json::from_value(json!({
        "id": ALLOCATED_VCPU,
        "quantity_basis": "allocated_capacity", "aggregation": "sum",
        "description": {"en": "Counts allocated vCPUs."}}))
    .unwrap()
}

#[test]
fn adapter_bounds_convert_exactly_into_each_plan_unit() {
    let limits = QuotaLimits {
        min: "0".into(),
        step: "1".into(),
        max: Some("1024".into()),
    };
    let gib = unit("gib", "1073741824", "1");
    let bytes = unit("bytes", "1", "1");
    assert_eq!(
        convert_bounds(&limits, &gib, &bytes).unwrap(),
        (
            "0".into(),
            "1073741824".into(),
            Some("1099511627776".into())
        )
    );
    assert_eq!(
        convert_bounds(&limits, &gib, &gib).unwrap(),
        ("0".into(), "1".into(), Some("1024".into()))
    );
}

#[test]
fn a_gib_step_is_inexact_in_gb() {
    let limits = QuotaLimits {
        min: "0".into(),
        step: "1".into(),
        max: None,
    };
    assert_eq!(
        convert_bounds(
            &limits,
            &unit("gib", "1073741824", "1"),
            &unit("gb", "1000000000", "1")
        ),
        Err(UnitError::Inexact)
    );
}

#[test]
fn a_capability_missing_an_operation_is_profile_unsupported() {
    let mut record = record_of(vcpu_record());
    record.operations.restrict = false;
    let r = check_profile(&record, &scope(), &accounting()).unwrap_err();
    assert_eq!(
        (r.code, r.field.as_str()),
        ("QUOTA_PROFILE_UNSUPPORTED", "operations.restrict")
    );
}

#[test]
fn initial_installation_is_required_and_increase_is_recorded() {
    let mut record = record_of(vcpu_record());
    record.change_support.increase = false;
    let profile = check_profile(&record, &scope(), &accounting()).unwrap();
    assert_eq!(profile.to_json()["change_support"]["increase"], false);
    assert_eq!(profile.to_json()["profile"], "bounded_allocation");
    assert_eq!(
        profile.to_json()["required_operations"],
        json!({"apply": true, "read_application": true, "recover_operation": true,
               "restrict": true, "revoke": true, "resume": true})
    );
    record.change_support.initial_set = false;
    assert_eq!(
        check_profile(&record, &scope(), &accounting())
            .unwrap_err()
            .field,
        "change_support.initial_set"
    );
}

#[test]
fn bases_and_units_are_non_empty_sets() {
    let en = BTreeMap::from([("en".to_owned(), "Counts stopped VMs".to_owned())]);
    assert_eq!(
        check_proposal_shape(&[], &[VCPU_UNIT.into()], &en, None)
            .unwrap_err()
            .field,
        "supported_bases"
    );
    assert_eq!(
        check_proposal_shape(
            &[QuotaBasis::Fixed, QuotaBasis::Fixed],
            &[VCPU_UNIT.into()],
            &en,
            None
        )
        .unwrap_err()
        .field,
        "supported_bases"
    );
    assert_eq!(
        check_proposal_shape(&[QuotaBasis::Fixed], &[], &en, None)
            .unwrap_err()
            .field,
        "allowed_unit_refs"
    );
    let nine: Vec<String> = (0..9).map(|i| format!("{VCPU_UNIT}{i}")).collect();
    assert_eq!(
        check_proposal_shape(&[QuotaBasis::Fixed], &nine, &en, None)
            .unwrap_err()
            .field,
        "allowed_unit_refs"
    );
    let twice = [VCPU_UNIT.to_owned(), VCPU_UNIT.to_owned()];
    assert_eq!(
        check_proposal_shape(&[QuotaBasis::Fixed], &twice, &en, None)
            .unwrap_err()
            .field,
        "allowed_unit_refs"
    );
    let long = "n".repeat(2001);
    let r = check_proposal_shape(&[QuotaBasis::Fixed], &[VCPU_UNIT.into()], &en, Some(&long))
        .unwrap_err();
    assert_eq!((r.code, r.field.as_str()), ("FIELD_TOO_LONG", "note"));
    let ru = BTreeMap::from([("ru".to_owned(), "Schitaet".to_owned())]);
    let r = check_proposal_shape(&[QuotaBasis::Fixed], &[VCPU_UNIT.into()], &ru, None).unwrap_err();
    assert_eq!(
        (r.code, r.field.as_str()),
        ("QUOTA_DISPLAY_TEXT_MISSING", "profile_conditions")
    );
    assert!(
        check_proposal_shape(
            &[QuotaBasis::Fixed, QuotaBasis::PerPlanUnit],
            &[VCPU_UNIT.into()],
            &en,
            Some("ok")
        )
        .is_ok()
    );
}

/// A stored proposal reads back as written; a member that does not decode names itself.
#[test]
fn a_stored_proposal_reads_back_as_written() {
    let vcpu: QuotaUnitV1 = serde_json::from_value(json!({
        "id": VCPU_UNIT, "symbol": "vCPU", "canonical_unit_ref": VCPU_UNIT,
        "numerator": "1", "denominator": "1"}))
    .unwrap();
    let proposal = Proposal {
        sku_id: uuid::Uuid::new_v4(),
        sku_version: QuotaVersion::new(3).unwrap(),
        definition_id: Some(uuid::Uuid::new_v4()),
        allowed_units: vec![AllowedUnit {
            unit: vcpu,
            evidence: Evidence {
                gts_id: VCPU_UNIT.to_owned(),
                uuid: uuid::Uuid::new_v4(),
                resource_version: 2,
                digest: "a".repeat(64),
            },
            min: "0".into(),
            step: "1".into(),
            max: None,
        }],
        supported_bases: vec![QuotaBasis::PerPlanUnit, QuotaBasis::Fixed],
        profile_conditions: BTreeMap::from([("en".to_owned(), "Stopped VMs count".to_owned())]),
    };
    let stored = proposal_json(&proposal);
    assert_eq!(stored["sku_version"], "3");
    assert_eq!(stored["supported_bases"], json!(["per_plan_unit", "fixed"]));
    assert_eq!(stored["allowed_units"][0]["max"], Value::Null);
    assert_eq!(proposal_from_json(&stored).unwrap(), proposal);
    let mut broken = stored;
    broken["allowed_units"][0]["digest"] = json!(7);
    assert!(
        proposal_from_json(&broken).unwrap_err().contains("digest"),
        "names the member"
    );
}
