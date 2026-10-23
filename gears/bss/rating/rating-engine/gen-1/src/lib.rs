//! BSS Rating engine generation 1 (rating DESIGN §4.1, ADR-0006).
//!
//! One frozen generation of the evaluation semantics: its own input and output types, the fixed
//! step order, the closed error taxonomy, the exact arithmetic and its budget, and the pure
//! functions the pipeline shares with it — [`split_points`], [`window_geometry`], the
//! [`DerivedMeterEvaluator`], the [`minimum_fee_floor`] roll-up applies and
//! [`evaluate_order`], the pure function under purchase evaluation. `rating-core`
//! re-exports it as `engine::gen_1` and dispatches to it by `engine_generation`; nothing in the
//! workspace depends on a generation's types except through that mapping.
//!
//! **Frozen.** Once released (by the migration that records [`ENGINE_DIGEST`] in
//! `bss_rating__engine_generation`, feature 07), any change of formulas, step order, arithmetic
//! rules, the [`budget::ArithmeticBudget`], numeric bounds or error classification is a new
//! generation crate, never an edit here. A refactor that keeps semantics must pass this
//! generation's golden vectors (`golden/`) byte for byte. Until it is released, generation 1 is
//! the generation under construction.
//!
//! **Dependencies.** Exact money is the shared `bss_money` crate itself (money ADR rule 2, T-D-84),
//! with the SHA-256 of its library sources recorded in `vendor/LOCK.toml`; the Products derived
//! evaluator is a renamed copy under `vendor/` (`vendor.toml`); the registry crates they compute
//! with are pinned by version and `Cargo.lock` checksum in the same lock until the freeze copies
//! them too.
//!
//! **Pure.** No I/O, no clock, no store and no hashing at run time: [`ENGINE_DIGEST`] is computed by
//! the build script over the frozen tree, and the pipeline computes `input_digest`.
#![forbid(unsafe_code)]

// @cpt-dod:cpt-cf-bss-rating-dod-core-evaluate:p1
// @cpt-dod:cpt-cf-bss-rating-dod-core-exact-and-replay:p1
// @cpt-dod:cpt-cf-bss-rating-dod-core-governance:p1
// @cpt-dod:cpt-cf-bss-rating-dod-core-pricing-semantics:p1
// @cpt-algo:cpt-cf-bss-rating-algo-core-engine-generations:p1

pub mod budget;
pub mod error;
pub mod exact;
pub mod geometry;
pub mod input;
pub mod meter;
pub mod model;
pub mod order;
pub mod outcome;

mod decimal_text;
mod exactness;
mod floor;
mod generation;
mod governance;
mod guards;
mod pipeline;
#[cfg(test)]
mod test_input;
mod validate;

pub use budget::ArithmeticBudget;
pub use error::EvaluationError;
pub use floor::minimum_fee_floor;
pub use generation::{ENGINE_DIGEST, GENERATION, engine_id};
pub use geometry::{FactExtent, SplitInput, split_points, window_geometry};
pub use input::{EngineId, EvaluationInput};
pub use meter::DerivedMeterEvaluator;
pub use order::{OrderEvaluation, OrderEvaluationInput, evaluate_order};
pub use outcome::{EvaluationOutcome, FloorPart};

/// Evaluates one child input into its outcome, or the first error of the fixed step order.
///
/// The same input gives the same outcome, byte for byte, on any worker at any later time. Nothing
/// is persisted, read or hashed.
///
/// # Errors
///
/// An [`EvaluationError`] of the closed taxonomy (rating DESIGN 01 §4.4): `engine_unavailable`
/// when the input names another generation, digest or budget, and otherwise the first failure of
/// validation, layout, binding, model, guards or bounds.
pub fn evaluate(input: &EvaluationInput) -> Result<EvaluationOutcome, EvaluationError> {
    pipeline::evaluate(input)
}
