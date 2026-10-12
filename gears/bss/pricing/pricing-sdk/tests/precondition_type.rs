//! The incomplete-input refusal names a stable type, not its Display text.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use bss_pricing_sdk::read::IncompleteCommercialInputs;
use toolkit_canonical_errors::{CanonicalError, Problem};

#[test]
fn incomplete_commercial_inputs_names_a_stable_violation_type() {
    let error: CanonicalError = IncompleteCommercialInputs {
        field: "sku_version",
    }
    .into();
    let problem = Problem::from_error(&error).unwrap();
    let body = serde_json::to_value(&problem).unwrap();
    let violation = &body["context"]["violations"][0];
    assert_eq!(violation["type"], "INCOMPLETE_COMMERCIAL_INPUTS");
    assert_eq!(violation["subject"], "sku_version");
    assert_eq!(
        violation["description"],
        "incomplete commercial inputs: sku_version"
    );
}
