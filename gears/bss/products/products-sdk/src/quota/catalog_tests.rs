//! The catalog client's wire types against the quotas package's example catalog answer
//! (P-D-272).
#![allow(clippy::unwrap_used)]
use super::*;
use serde_json::{Value, json};

/// The package's example catalog answer: a REST list, whose bases are the REST tokens.
fn example() -> Value {
    serde_json::from_str(include_str!(
        "../../../../docs/quotas/schemas/quota-catalog-list.example.json"
    ))
    .unwrap()
}

/// The example's row as the SDK carries it: the REST tokens `fixed` and `per_plan_unit` become the
/// package's `Fixed` and `PerPlanUnit` (P-D-272); every other member is as the example writes it.
fn example_row() -> Value {
    let mut row = example()["items"][0].clone();
    let bases: Vec<Value> = row["supported_bases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|token| match token.as_str().unwrap() {
            "fixed" => json!("Fixed"),
            "per_plan_unit" => json!("PerPlanUnit"),
            other => panic!("the REST example names no basis {other}"),
        })
        .collect();
    row["supported_bases"] = Value::Array(bases);
    row
}

#[test]
fn quota_version_round_trips_as_a_decimal_string() {
    let v: QuotaVersion = serde_json::from_value(json!("7")).unwrap();
    assert_eq!(v.get(), 7);
    assert_eq!(serde_json::to_value(v).unwrap(), json!("7"));
    for bad in [json!("0"), json!("07"), json!("-1"), json!(7)] {
        assert!(
            serde_json::from_value::<QuotaVersion>(bad.clone()).is_err(),
            "{bad}"
        );
    }
    assert_eq!(QuotaVersion::new(0), Err(QuotaVersionError));
    assert_eq!(QuotaVersion::new(7).unwrap(), v);
    assert_eq!("+7".parse::<QuotaVersion>(), Err(QuotaVersionError));
}

#[test]
fn the_package_example_row_deserializes_into_a_definition() {
    let definition: QuotaDefinition = serde_json::from_value(example_row()).unwrap();
    assert!(definition.qualification_ref.is_none());
    assert_eq!(
        definition.availability.reason_code.as_deref(),
        Some("qualification_pending")
    );
    assert_eq!(
        definition.availability.state,
        QuotaAvailabilityState::Disabled
    );
    assert_eq!(definition.sku_version.get(), 3);
    assert_eq!(definition.allowed_units[0].max.as_deref(), Some("1024"));
}

#[test]
fn a_definition_serializes_its_bases_as_the_package_spells_them() {
    assert_eq!(
        example()["items"][0]["supported_bases"],
        json!(["fixed", "per_plan_unit"]),
        "the REST example spells the bases as Products REST does"
    );
    let definition: QuotaDefinition = serde_json::from_value(example_row()).unwrap();
    assert_eq!(
        definition.supported_bases,
        vec![QuotaBasis::Fixed, QuotaBasis::PerPlanUnit]
    );
    let written = serde_json::to_value(&definition).unwrap();
    assert_eq!(written["supported_bases"], json!(["Fixed", "PerPlanUnit"]));
}

#[test]
fn the_package_example_row_writes_back_unchanged() {
    let row = example_row();
    let definition: QuotaDefinition = serde_json::from_value(row.clone()).unwrap();
    assert_eq!(serde_json::to_value(&definition).unwrap(), row);
}
