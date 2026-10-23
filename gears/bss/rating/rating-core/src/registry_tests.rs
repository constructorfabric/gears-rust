#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::{
    ArithmeticBudget, CoreUsagePolicy, Engine, EngineId, EngineRegistry, EvaluationError,
    EvaluationInput, EvaluationOutcome, FactExtent, FloorPart, Gen1, OrderEvaluation,
    OrderEvaluationInput, PriceTotal, RatingWindow, RegistryError, SplitInput, gen_1,
};

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

#[path = "../tests/support/later.rs"]
mod later;

use later::Later;

#[test]
fn every_operation_dispatches_to_its_generation() {
    let registry = EngineRegistry::compiled();
    let engine = registry.current();
    let input = golden_input("ends-on-handover");
    let split = engine.split_points(&gen_1::SplitInput {
        kind: input.child.kind,
        window: input.child.window,
        ends_on: vec![time::macros::date!(2026 - 09 - 11)],
        quantity_intervals: Vec::new(),
    });
    assert_eq!(split, [time::macros::datetime!(2026-09-11 00:00:00 UTC)]);
    let policy = golden_input("f23-hourly-volume-q8").pricing.resolves[0].cells[0]
        .binding
        .clone()
        .unwrap()
        .usage_policy
        .unwrap();
    let windows = engine
        .window_geometry(
            &policy,
            gen_1::FactExtent {
                served: input.fact.served,
                period: input.fact.period,
            },
        )
        .unwrap();
    assert_eq!(windows.len(), 720);
    let totals = gen_1::evaluate(&input).unwrap().price_totals;
    let floor = engine.minimum_fee_floor(&totals).unwrap();
    // The successor price carries a minimum fee of 45 over 20 of 30 days.
    assert_eq!(floor.len(), 1);
    assert_eq!(floor[0].floor.numerator, "30");
}

#[test]
fn a_registry_prints_its_generations() {
    let printed = format!("{:?}", EngineRegistry::compiled());
    assert!(printed.starts_with("EngineRegistry"), "{printed}");
    assert!(printed.contains(gen_1::ENGINE_DIGEST), "{printed}");
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
    // Two retained engines of one older generation: the second never silently replaces the first.
    let retained_twice = EngineRegistry::from_engines(
        Box::new(Later(3)),
        vec![Box::new(Later(2)), Box::new(Later(2))],
    );
    assert_eq!(
        retained_twice.err(),
        Some(RegistryError::DuplicateGeneration(2))
    );
    let newer = EngineRegistry::from_engines(Box::new(Gen1), vec![Box::new(Later(3))]);
    assert_eq!(
        newer.err(),
        Some(RegistryError::RetainedNotOlder {
            retained: 3,
            current: 1
        })
    );
}
