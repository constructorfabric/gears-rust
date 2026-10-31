#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::{DecodeError, EvaluationInput, decode_input, input_json, outcome_json};
use crate::EngineRegistry;

/// A golden vector's input as JSON, with the compiled engine digest in place.
fn golden_json(name: &str) -> serde_json::Value {
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

fn golden_input(name: &str) -> EvaluationInput {
    serde_json::from_value(golden_json(name)).unwrap()
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
fn an_absent_optional_price_field_reads_as_null() {
    let mut json = golden_json("worked-b-monthly-graduated");
    let text = input_json(&serde_json::from_value(json.clone()).unwrap()).unwrap();
    let price = &mut json["pricing"]["resolves"][0]["cells"][0]["binding"]["price"];
    for key in ["minimum_fee", "ends_on"] {
        price.as_object_mut().unwrap().remove(key).unwrap();
    }
    price["model"]["tiers"][1]
        .as_object_mut()
        .unwrap()
        .remove("up_to")
        .unwrap();
    let input: EvaluationInput = serde_json::from_value(json).unwrap();
    assert_eq!(input_json(&input).unwrap(), text);
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

#[test]
fn an_enum_value_the_generation_does_not_know_is_unknown_enum_value() {
    let text = input_json(&golden_input("f23-hourly-volume-q8")).unwrap();
    assert_eq!(
        decode_input(&text).unwrap(),
        golden_input("f23-hourly-volume-q8")
    );
    for (known, unknown) in [
        (r#""model":"volume""#, r#""model":"stepped""#),
        (
            r#""kind":"usage","recurring_period""#,
            r#""kind":"prepaid","recurring_period""#,
        ),
        (
            r#""rating_window":"calendar_hour_utc""#,
            r#""rating_window":"calendar_week""#,
        ),
    ] {
        assert!(text.contains(known), "{known}");
        let changed = text.replacen(known, unknown, 1);
        let Err(DecodeError::Unknown(error)) = decode_input(&changed) else {
            panic!("{unknown} decoded");
        };
        assert_eq!(error.reason_code(), "unknown_enum_value");
    }
    assert!(matches!(
        decode_input("{\"child\": 1}"),
        Err(DecodeError::Malformed(_))
    ));
}
