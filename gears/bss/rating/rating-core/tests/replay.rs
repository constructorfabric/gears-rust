//! Cross-upgrade replay (rating ADR-0006 "Confirmation", acceptance vector V22).
//!
//! Every result of generation 1's golden corpus is replayed in a build whose current generation is
//! a later one, and must re-execute byte for byte under generation 1; the registry never moves it
//! to the current generation, and a generation that is not compiled in, or compiled in with
//! another digest, is `engine_unavailable`.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};

use bss_rating_core::engine::gen_1;
use bss_rating_core::{
    ArithmeticBudget, CoreUsagePolicy, Engine, EngineId, EngineRegistry, EvaluationError,
    EvaluationInput, EvaluationOutcome, FactExtent, FloorPart, Gen1, OrderEvaluation,
    OrderEvaluationInput, PriceTotal, RatingWindow, SplitInput,
};
use serde::Serialize;

const DIGEST_TOKEN: &str = "<engine_digest>";

#[path = "support/later.rs"]
mod later;

use later::Later;

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Recorded {
    Outcome(EvaluationOutcome),
    Order(OrderEvaluation),
    Floor(Vec<FloorPart>),
    Error(EvaluationError),
}

/// A recorded case, decoded for the entry point it names (`evaluate` when it names none).
enum Recording {
    Evaluate(Box<EvaluationInput>),
    Order(OrderEvaluationInput),
    /// The floor's input carries no engine: it is replayed by the generation of the fact's
    /// children, here generation 1.
    Floor(Vec<PriceTotal>),
}

fn corpus() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rating-engine/gen-1/golden");
    let mut cases: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.to_string_lossy().ends_with(".input.json"))
        .collect();
    cases.sort();
    assert!(!cases.is_empty());
    cases
}

fn recorded_input(path: &Path) -> Recording {
    let case: serde_json::Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    let live = case["input"]
        .to_string()
        .replace(DIGEST_TOKEN, gen_1::ENGINE_DIGEST);
    match case["entry"].as_str() {
        None => Recording::Evaluate(Box::new(serde_json::from_str(&live).unwrap())),
        Some("evaluate_order") => Recording::Order(serde_json::from_str(&live).unwrap()),
        Some("minimum_fee_floor") => Recording::Floor(serde_json::from_str(&live).unwrap()),
        Some(other) => panic!("{}: unknown entry {other}", path.display()),
    }
}

/// The generation a recording names; a floor's is generation 1's.
fn named(recording: &Recording) -> EngineId {
    match recording {
        Recording::Evaluate(input) => input.engine.clone(),
        Recording::Order(input) => input.engine.clone(),
        Recording::Floor(_) => gen_1::engine_id(),
    }
}

fn recorded_result(path: &Path) -> String {
    fs::read_to_string(
        path.to_string_lossy()
            .replace(".input.json", ".expected.json"),
    )
    .unwrap()
}

fn replayed(registry: &EngineRegistry, recording: &Recording) -> String {
    let result = match recording {
        Recording::Evaluate(input) => registry.evaluate(input).map(Recorded::Outcome),
        Recording::Order(input) => registry.evaluate_order(input).map(Recorded::Order),
        Recording::Floor(totals) => registry
            .engine_for(&named(recording))
            .and_then(|engine| engine.minimum_fee_floor(totals))
            .map(Recorded::Floor),
    }
    .unwrap_or_else(Recorded::Error);
    serde_json::to_string_pretty(&result)
        .unwrap()
        .replace(gen_1::ENGINE_DIGEST, DIGEST_TOKEN)
        + "\n"
}

#[test]
fn v22_generation_one_results_replay_identically_when_generation_two_is_current() {
    let registry = EngineRegistry::from_engines(Box::new(Later(2)), vec![Box::new(Gen1)]).unwrap();
    assert_eq!(registry.current().id().engine_generation, 2);
    for path in corpus() {
        let recording = recorded_input(&path);
        assert_eq!(named(&recording).engine_generation, 1);
        assert_eq!(
            replayed(&registry, &recording),
            recorded_result(&path),
            "{}",
            path.display()
        );
    }
}

#[test]
fn v22_a_generation_that_is_not_retained_is_engine_unavailable() {
    let without_gen_1 = EngineRegistry::from_engines(Box::new(Later(2)), Vec::new()).unwrap();
    for path in corpus() {
        let replay: serde_json::Value =
            serde_json::from_str(&replayed(&without_gen_1, &recorded_input(&path))).unwrap();
        assert_eq!(
            replay["error"]["reason_code"],
            "engine_unavailable",
            "{}: {replay}",
            path.display()
        );
    }
}

#[test]
fn a_result_recorded_under_another_digest_is_never_replayed_by_this_build() {
    let registry = EngineRegistry::from_engines(Box::new(Later(2)), vec![Box::new(Gen1)]).unwrap();
    let Recording::Evaluate(mut input) = recorded_input(&corpus()[0]) else {
        panic!("the first golden vector calls evaluate");
    };
    input.engine.engine_digest = "f".repeat(64);
    assert_eq!(
        registry.evaluate(&input).unwrap_err().reason_code(),
        "engine_unavailable"
    );
}
