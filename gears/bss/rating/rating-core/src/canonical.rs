//! The canonical serialization of evaluation inputs and outcomes (DESIGN 01 §3.2 `Digest`,
//! DESIGN 14 §4.3).
//!
//! The pipeline hashes the canonical input into `input_digest`, so the same input has one text:
//! fields in declaration order, no whitespace, every number as the canonical text of §3.3 (a
//! quantity `10.000` is written `10`, an amount as its reduced fraction), every instant as a whole
//! UTC second in RFC 3339. The core itself hashes nothing.

use serde::Serialize;

use crate::{EvaluationInput, EvaluationOutcome};

/// The canonical JSON text of an input.
///
/// # Errors
///
/// A serialization error, which a valid input never raises.
pub fn input_json(input: &EvaluationInput) -> Result<String, serde_json::Error> {
    to_canonical(input)
}

/// The canonical JSON text of an outcome: two evaluations of one input give the same bytes.
///
/// # Errors
///
/// A serialization error, which a valid outcome never raises.
pub fn outcome_json(outcome: &EvaluationOutcome) -> Result<String, serde_json::Error> {
    to_canonical(outcome)
}

fn to_canonical<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    serde_json::to_string(value)
}

#[cfg(test)]
#[path = "canonical_tests.rs"]
mod tests;
