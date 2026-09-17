//! Pure admission rules:
//! - `baseline`: select one baseline and interpret the resolved comparison.
//! - `derivation`: enforce major-0 quarantine and pin the dialect across a major.
//!
//! Acceptance shares the Draft-07 spelling set with the dialect pin.

mod baseline;
mod derivation;
mod sides;

pub(crate) use baseline::{
    Baseline, UnreadableVersion, backward_comparison, refusal, select_baseline,
};
pub(crate) use derivation::{DialectDrift, dialect_pin, normalize_dialect, quarantine};
pub(crate) use sides::{BaselineDoc, CandidateDoc};
