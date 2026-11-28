//! The quota wire types (P-D-272): every REST enum mirrors its SDK or domain enum, variant by
//! variant, both ways, with the tokens the wire carries.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::dto::*;
use crate::domain::quota::{CandidateOrigin, CandidateState};
use bss_products_sdk::quota::catalog::{QuotaAvailabilityState, QuotaBasis};
use bss_products_sdk::quota::gts::QuotaKind;

fn token<T: serde::Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn every_sdk_enum_maps_to_its_rest_mirror() {
    for (sdk, wire) in [
        (QuotaAvailabilityState::Enabled, "enabled"),
        (QuotaAvailabilityState::Disabled, "disabled"),
        (QuotaAvailabilityState::Withdrawn, "withdrawn"),
    ] {
        let rest = ProductsQuotaAvailabilityState::from(sdk);
        assert_eq!(QuotaAvailabilityState::from(rest), sdk);
        assert_eq!(token(rest), wire);
    }
    for (sdk, wire) in [
        (QuotaBasis::Fixed, "fixed"),
        (QuotaBasis::PerPlanUnit, "per_plan_unit"),
    ] {
        let rest = ProductsQuotaBasis::from(sdk);
        assert_eq!(QuotaBasis::from(rest), sdk);
        assert_eq!(token(rest), wire);
    }
    for (domain, wire) in [
        (CandidateState::Staged, "staged"),
        (CandidateState::Submitted, "submitted"),
        (CandidateState::Approved, "approved"),
        (CandidateState::Rejected, "rejected"),
    ] {
        let rest = ProductsQuotaCandidateState::from(domain);
        assert_eq!(CandidateState::from(rest), domain);
        assert_eq!(token(rest), wire);
        assert_eq!(wire, domain.as_str(), "the wire token is the stored one");
    }
    let rest = ProductsQuotaKind::from(QuotaKind::Allocation);
    assert_eq!(QuotaKind::from(rest), QuotaKind::Allocation);
    assert_eq!(token(rest), "allocation");
    assert_eq!(token(rest), token(QuotaKind::Allocation));
    for (domain, wire) in [
        (CandidateOrigin::Discovery, "discovery"),
        (CandidateOrigin::Manual, "manual"),
    ] {
        let rest = ProductsQuotaCandidateOrigin::from(domain);
        assert_eq!(CandidateOrigin::from(rest), domain);
        assert_eq!(token(rest), wire);
        assert_eq!(wire, domain.as_str(), "the wire token is the stored one");
    }
}

/// The quotas package's example catalog answer is a REST list: its row is what the REST
/// definition writes for the same SDK definition, the bases as `fixed` and `per_plan_unit`.
#[test]
fn the_package_example_row_is_the_rest_definition() {
    use bss_products_sdk::quota::catalog::QuotaDefinition;
    use serde_json::{Value, json};
    let example: Value = serde_json::from_str(include_str!(
        "../../../../../../docs/quotas/schemas/quota-catalog-list.example.json"
    ))
    .unwrap();
    let row = example["items"][0].clone();
    let mut sdk_row = row.clone();
    sdk_row["supported_bases"] = json!(["Fixed", "PerPlanUnit"]);
    assert_eq!(row["supported_bases"], json!(["fixed", "per_plan_unit"]));
    let definition: QuotaDefinition = serde_json::from_value(sdk_row).unwrap();
    let rest = serde_json::to_value(ProductsQuotaDefinition::from(definition)).unwrap();
    assert_eq!(rest, row);
}
