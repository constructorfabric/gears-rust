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
    ArithmeticBudget, Engine, EngineId, EngineRegistry, EvaluationError, EvaluationInput,
    EvaluationOutcome, Gen1,
};
use serde::Serialize;

const DIGEST_TOKEN: &str = "<engine_digest>";

/// A later current generation with different semantics: it rates every line as nothing, so a
/// replay that reached it would not reproduce generation 1's results.
struct Gen2;

impl Engine for Gen2 {
    fn id(&self) -> EngineId {
        EngineId {
            engine_generation: 2,
            engine_digest: "2".repeat(64),
        }
    }

    fn budget(&self) -> ArithmeticBudget {
        ArithmeticBudget::GEN_1
    }

    fn evaluate(&self, _: &EvaluationInput) -> Result<EvaluationOutcome, EvaluationError> {
        Ok(EvaluationOutcome {
            lines: Vec::new(),
            obligations: Vec::new(),
            price_totals: Vec::new(),
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Recorded {
    Outcome(EvaluationOutcome),
    Error(EvaluationError),
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

fn recorded_input(path: &Path) -> EvaluationInput {
    let case: serde_json::Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    serde_json::from_str(
        &case["input"]
            .to_string()
            .replace(DIGEST_TOKEN, gen_1::ENGINE_DIGEST),
    )
    .unwrap()
}

fn recorded_result(path: &Path) -> String {
    fs::read_to_string(
        path.to_string_lossy()
            .replace(".input.json", ".expected.json"),
    )
    .unwrap()
}

fn replayed(registry: &EngineRegistry, input: &EvaluationInput) -> String {
    let result = match registry.evaluate(input) {
        Ok(outcome) => Recorded::Outcome(outcome),
        Err(error) => Recorded::Error(error),
    };
    serde_json::to_string_pretty(&result)
        .unwrap()
        .replace(gen_1::ENGINE_DIGEST, DIGEST_TOKEN)
        + "\n"
}

#[test]
fn v22_generation_one_results_replay_identically_when_generation_two_is_current() {
    let registry = EngineRegistry::from_engines(Box::new(Gen2), vec![Box::new(Gen1)]).unwrap();
    assert_eq!(registry.current().id().engine_generation, 2);
    for path in corpus() {
        let input = recorded_input(&path);
        assert_eq!(input.engine.engine_generation, 1);
        assert_eq!(
            replayed(&registry, &input),
            recorded_result(&path),
            "{}",
            path.display()
        );
    }
}

#[test]
fn v22_a_generation_that_is_not_retained_is_engine_unavailable() {
    let without_gen_1 = EngineRegistry::from_engines(Box::new(Gen2), Vec::new()).unwrap();
    for path in corpus() {
        let error = without_gen_1.evaluate(&recorded_input(&path)).unwrap_err();
        assert_eq!(
            error.reason_code(),
            "engine_unavailable",
            "{}",
            path.display()
        );
    }
}

#[test]
fn a_result_recorded_under_another_digest_is_never_replayed_by_this_build() {
    let registry = EngineRegistry::from_engines(Box::new(Gen2), vec![Box::new(Gen1)]).unwrap();
    let mut input = recorded_input(&corpus()[0]);
    input.engine.engine_digest = "f".repeat(64);
    assert_eq!(
        registry.evaluate(&input).unwrap_err().reason_code(),
        "engine_unavailable"
    );
}
