//! Generation 1's golden corpus (rating ADR-0006, feature 02 §6).
//!
//! Every `golden/<case>.input.json` is evaluated and its result compared byte for byte with
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

use bss_rating_engine_gen_1::{ENGINE_DIGEST, EvaluationError, EvaluationInput, EvaluationOutcome};
use serde::{Deserialize, Serialize};

const DIGEST_TOKEN: &str = "<engine_digest>";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    #[expect(dead_code, reason = "read by people, checked to be present")]
    description: String,
    #[expect(dead_code, reason = "read by people, checked to be present")]
    vectors: Vec<String>,
    input: serde_json::Value,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Expected {
    Outcome(EvaluationOutcome),
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

/// The input of a case, with the live digest in place of the token.
fn input(path: &Path) -> (EvaluationInput, serde_json::Value) {
    let text = fs::read_to_string(path).unwrap();
    let case: Case = serde_json::from_str(&text).unwrap();
    let live = serde_json::to_string(&case.input)
        .unwrap()
        .replace(DIGEST_TOKEN, ENGINE_DIGEST);
    let input: EvaluationInput =
        serde_json::from_str(&live).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    (input, serde_json::from_str(&live).unwrap())
}

fn result(input: &EvaluationInput) -> String {
    let expected = match bss_rating_engine_gen_1::evaluate(input) {
        Ok(outcome) => Expected::Outcome(outcome),
        Err(error) => Expected::Error(error),
    };
    serde_json::to_string_pretty(&expected)
        .unwrap()
        .replace(ENGINE_DIGEST, DIGEST_TOKEN)
        + "\n"
}

// @cpt-begin:cpt-cf-bss-rating-algo-core-engine-generations:p1:inst-eg-corpus
#[test]
fn every_golden_vector_reproduces_its_recorded_result() {
    let bless = std::env::var("RATING_GOLDEN_BLESS").is_ok_and(|value| value == "1");
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
        let copy: EvaluationInput = serde_json::from_value(document).unwrap();
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
