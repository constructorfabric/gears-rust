#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;

/// A golden input of generation 1, with the live digest in place of its token.
fn golden_input(name: &str) -> EvaluationInput {
    let path = format!(
        "{}/../rating-engine/gen-1/golden/{name}.input.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let case: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let text = case["input"]
        .to_string()
        .replace("<engine_digest>", gen_1::ENGINE_DIGEST);
    serde_json::from_str(&text).unwrap()
}

/// A stand-in for a later generation that rates everything as nothing.
struct Later(u32);

impl Engine for Later {
    fn id(&self) -> EngineId {
        EngineId {
            engine_generation: self.0,
            engine_digest: format!("{:064x}", self.0),
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

#[test]
fn this_build_compiles_generation_one_as_current() {
    let registry = EngineRegistry::compiled();
    assert_eq!(registry.current().id(), gen_1::engine_id());
    assert_eq!(registry.generations(), vec![gen_1::engine_id()]);
    assert!(registry.get(1).is_some());
    assert!(registry.get(2).is_none());
    assert_eq!(registry.current().budget(), ArithmeticBudget::GEN_1);
}

#[test]
fn dispatch_follows_the_recorded_generation() {
    let registry = EngineRegistry::from_engines(Box::new(Later(2)), vec![Box::new(Gen1)]).unwrap();
    let input = golden_input("v35-per-unit-0.047");
    let outcome = registry.evaluate(&input).unwrap();
    assert_eq!(outcome.lines[0].exact_amount.denominator, "400");
    assert_eq!(registry.current().id().engine_generation, 2);
    assert_eq!(
        registry
            .generations()
            .iter()
            .map(|id| id.engine_generation)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
}

#[test]
fn a_missing_generation_or_digest_is_never_substituted() {
    let registry = EngineRegistry::compiled();
    let mut input = golden_input("v35-per-unit-0.047");
    input.engine.engine_generation = 7;
    let error = registry.evaluate(&input).unwrap_err();
    assert_eq!(error.reason_code(), "engine_unavailable");

    let mut input = golden_input("v35-per-unit-0.047");
    input.engine.engine_digest = "00".repeat(32);
    let error = registry.evaluate(&input).unwrap_err();
    assert_eq!(error.reason_code(), "engine_unavailable");
    assert!(error.detail().contains("not the recorded"));
}

#[test]
fn a_registry_has_one_engine_per_generation_and_the_newest_is_current() {
    let twice = EngineRegistry::from_engines(Box::new(Gen1), vec![Box::new(Gen1)]);
    assert_eq!(twice.err(), Some(RegistryError::DuplicateGeneration(1)));
    let newer = EngineRegistry::from_engines(Box::new(Gen1), vec![Box::new(Later(3))]);
    assert_eq!(
        newer.err(),
        Some(RegistryError::RetainedNotOlder {
            retained: 3,
            current: 1
        })
    );
}
