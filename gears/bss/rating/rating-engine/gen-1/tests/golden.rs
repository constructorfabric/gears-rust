//! Generation 1's golden corpus (rating ADR-0006, feature 02 §6).
//!
//! Every `golden/<case>.input.json` is run through its entry point (`evaluate`, or the `entry`
//! it names: `evaluate_order`, `minimum_fee_floor`) and its result compared byte for byte with
//! `golden/<case>.expected.json`. Both files are part of the frozen tree: a refactor that keeps
//! semantics passes them unchanged, and a semantic change is a new generation. The engine digest
//! is written as `<engine_digest>` in both, since the digest covers the files themselves.
//!
//! `RATING_GOLDEN_BLESS=1 cargo test -p cf-gears-bss-rating-engine-gen-1 --test golden` rewrites
//! the expected files (only the value `1` does); only a generation that is not yet released may
//! do so.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};

use bss_rating_engine_gen_1::outcome::{FloorPart, PriceTotal};
use bss_rating_engine_gen_1::{
    ENGINE_DIGEST, EvaluationError, EvaluationInput, EvaluationOutcome, OrderEvaluation,
    OrderEvaluationInput,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

const DIGEST_TOKEN: &str = "<engine_digest>";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    #[expect(dead_code, reason = "read by people, checked to be present")]
    description: String,
    #[expect(dead_code, reason = "read by people, checked to be present")]
    vectors: Vec<String>,
    /// The entry point the vector calls; `evaluate` when absent.
    #[serde(default)]
    entry: Entry,
    input: serde_json::Value,
}

/// The generation's money-producing entry points.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Entry {
    #[default]
    Evaluate,
    EvaluateOrder,
    MinimumFeeFloor,
}

/// A vector's input, decoded for its entry point.
#[derive(Serialize)]
#[serde(untagged)]
enum Input {
    Evaluate(Box<EvaluationInput>),
    Order(OrderEvaluationInput),
    Floor(Vec<PriceTotal>),
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Expected {
    Outcome(EvaluationOutcome),
    Order(OrderEvaluation),
    Floor(Vec<FloorPart>),
    Error(EvaluationError),
}

fn corpus() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("golden");
    let mut cases: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.to_string_lossy().ends_with(".input.json"))
        .collect();
    cases.sort();
    assert!(!cases.is_empty(), "no golden vectors in {}", dir.display());
    cases
}

/// The input of a case, with the live digest in place of the token, and its JSON document.
fn input(path: &Path) -> (Input, serde_json::Value) {
    let text = fs::read_to_string(path).unwrap();
    let case: Case = serde_json::from_str(&text).unwrap();
    let live = serde_json::to_string(&case.input)
        .unwrap()
        .replace(DIGEST_TOKEN, ENGINE_DIGEST);
    let input = match case.entry {
        Entry::Evaluate => Input::Evaluate(Box::new(decode(path, &live))),
        Entry::EvaluateOrder => Input::Order(decode(path, &live)),
        Entry::MinimumFeeFloor => Input::Floor(decode(path, &live)),
    };
    (input, serde_json::from_str(&live).unwrap())
}

/// `live` decoded as the entry point's input type.
fn decode<T: DeserializeOwned>(path: &Path, live: &str) -> T {
    serde_json::from_str(live).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// The recorded form of an entry point's result, with the digest written as the token.
fn result(input: &Input) -> String {
    let expected = match input {
        Input::Evaluate(input) => bss_rating_engine_gen_1::evaluate(input).map(Expected::Outcome),
        Input::Order(input) => bss_rating_engine_gen_1::evaluate_order(input).map(Expected::Order),
        Input::Floor(totals) => {
            bss_rating_engine_gen_1::minimum_fee_floor(totals).map(Expected::Floor)
        }
    }
    .unwrap_or_else(Expected::Error);
    serde_json::to_string_pretty(&expected)
        .unwrap()
        .replace(ENGINE_DIGEST, DIGEST_TOKEN)
        + "\n"
}

/// The same document decoded again, for the entry point `like` was decoded for.
fn decode_like(like: &Input, document: serde_json::Value) -> Input {
    match like {
        Input::Evaluate(_) => Input::Evaluate(Box::new(serde_json::from_value(document).unwrap())),
        Input::Order(_) => Input::Order(serde_json::from_value(document).unwrap()),
        Input::Floor(_) => Input::Floor(serde_json::from_value(document).unwrap()),
    }
}

// @cpt-begin:cpt-cf-bss-rating-algo-core-engine-generations:p1:inst-eg-corpus
#[test]
fn every_golden_vector_reproduces_its_recorded_result() {
    let bless = std::env::var("RATING_GOLDEN_BLESS").is_ok_and(|value| value == "1");
    // Blessing compares nothing, so it never runs where the corpus is the gate.
    assert!(
        !(bless && std::env::var_os("CI").is_some()),
        "RATING_GOLDEN_BLESS is set in CI: the corpus would be rewritten instead of compared"
    );
    for path in corpus() {
        let (input, _) = input(&path);
        let actual = result(&input);
        let expected_path = PathBuf::from(
            path.to_string_lossy()
                .replace(".input.json", ".expected.json"),
        );
        if bless {
            fs::write(&expected_path, &actual).unwrap();
            continue;
        }
        let expected = fs::read_to_string(&expected_path)
            .unwrap_or_else(|error| panic!("{}: {error}", expected_path.display()));
        assert_eq!(actual, expected, "{}", path.display());
    }
}
// @cpt-end:cpt-cf-bss-rating-algo-core-engine-generations:p1:inst-eg-corpus

#[test]
fn the_same_input_gives_byte_identical_outcomes() {
    for path in corpus() {
        let (input, document) = input(&path);
        let copy = decode_like(&input, document);
        assert_eq!(result(&input), result(&copy), "{}", path.display());
    }
}

#[test]
fn every_golden_input_round_trips_without_loss() {
    for path in corpus() {
        let (input, document) = input(&path);
        assert_eq!(
            serde_json::to_value(&input).unwrap(),
            document,
            "{}",
            path.display()
        );
    }
}
