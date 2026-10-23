#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;
use crate::EngineRegistry;

fn golden_input(name: &str) -> EvaluationInput {
    let path = format!(
        "{}/../rating-engine/gen-1/golden/{name}.input.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let case: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let text = case["input"]
        .to_string()
        .replace("<engine_digest>", crate::engine::gen_1::ENGINE_DIGEST);
    serde_json::from_str(&text).unwrap()
}

#[test]
fn one_input_has_one_canonical_text() {
    let input = golden_input("v38-three-thirds");
    let text = input_json(&input).unwrap();
    let reread: EvaluationInput = serde_json::from_str(&text).unwrap();
    assert_eq!(input_json(&reread).unwrap(), text);
    assert!(!text.contains('\n') && !text.contains("\": ") && !text.contains(", \""));
    assert!(text.contains(r#""window_start":"2026-09-01T00:00:00Z""#));
}

#[test]
fn trailing_zeros_of_a_quantity_do_not_change_the_text() {
    let mut padded = golden_input("v35-per-unit-0.047");
    let text = input_json(&padded).unwrap();
    padded.quantities.slices[0].q = serde_json::from_str(r#""2.500""#).unwrap();
    assert_eq!(input_json(&padded).unwrap(), text);
}

#[test]
fn the_same_input_gives_the_same_outcome_bytes() {
    let registry = EngineRegistry::compiled();
    let input = golden_input("ends-on-handover");
    let first = outcome_json(&registry.evaluate(&input).unwrap()).unwrap();
    let reread: EvaluationInput = serde_json::from_str(&input_json(&input).unwrap()).unwrap();
    let again = outcome_json(&registry.evaluate(&reread).unwrap()).unwrap();
    assert_eq!(first, again);
}
