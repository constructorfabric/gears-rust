//! The canonical serialization of evaluation inputs and outcomes (DESIGN 01 §3.2 `Digest`,
//! DESIGN 14 §4.3).
//!
//! The pipeline hashes the canonical input into `input_digest`, so the same input has one text:
//! fields in declaration order, no whitespace, every number as the canonical text of §3.3 (a
//! quantity `10.000` is written `10`, an amount as its reduced fraction), every instant as a whole
//! UTC second in RFC 3339. The core itself hashes nothing.
//!
//! Decoding is the other direction (DESIGN 01 §3.1): the pipeline's canonical text becomes the
//! dispatched generation's types, and an enum value that generation does not know — a price model,
//! charge kind or rating window added after it was frozen — is its `unknown_enum_value`, never a
//! fallback.

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{EvaluationError, EvaluationInput, EvaluationOutcome, OrderEvaluationInput};

/// Why a text does not decode into the generation's input.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    /// A value the generation does not know: its `unknown_enum_value`.
    #[error(transparent)]
    Unknown(EvaluationError),
    /// Text that is not an input of this generation at all.
    #[error("not an input of this engine generation: {0}")]
    Malformed(serde_json::Error),
}

/// Decodes canonical input text into the generation's `EvaluationInput`.
///
/// # Errors
///
/// [`DecodeError::Unknown`] (`unknown_enum_value`) for an enum value the generation does not know,
/// [`DecodeError::Malformed`] for anything else that does not decode.
pub fn decode_input(text: &str) -> Result<EvaluationInput, DecodeError> {
    decode(text)
}

/// Decodes canonical order text into the generation's `OrderEvaluationInput`.
///
/// # Errors
///
/// As [`decode_input`].
pub fn decode_order(text: &str) -> Result<OrderEvaluationInput, DecodeError> {
    decode(text)
}

/// `text` as `T`, an unknown enum value told apart from other errors.
fn decode<T: DeserializeOwned>(text: &str) -> Result<T, DecodeError> {
    serde_json::from_str(text).map_err(|error| {
        // serde reports a value outside a closed enum as `unknown variant `…`, expected …`.
        if error.is_data() && error.to_string().starts_with("unknown variant") {
            DecodeError::Unknown(EvaluationError::UnknownEnumValue {
                detail: error.to_string(),
            })
        } else {
            DecodeError::Malformed(error)
        }
    })
}

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

/// Compact JSON in field declaration order: the one text of a value whose fields are canonical.
fn to_canonical<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    serde_json::to_string(value)
}

#[cfg(test)]
#[path = "canonical_tests.rs"]
mod tests;
