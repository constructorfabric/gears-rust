//! BSS Rating engine generation 1 (rating DESIGN §4.1, ADR-0006).
//!
//! One frozen generation of the evaluation semantics: its own input and output types, the fixed
//! step order, the closed error taxonomy, the exact arithmetic and its budget. `rating-core`
//! re-exports it as `engine::gen_1` and dispatches to it by `engine_generation`; nothing in the
//! workspace depends on a generation's types except through that mapping.
//!
//! **Frozen.** Once released (by the migration that records [`ENGINE_DIGEST`] in
//! `bss_rating__engine_generation`, feature 07), any change of formulas, step order, arithmetic
//! rules, the [`budget::ArithmeticBudget`], numeric bounds or error classification is a new
//! generation crate, never an edit here. A refactor that keeps semantics must pass this
//! generation's golden vectors (`golden/`) byte for byte. Generation 1 is frozen once R-35 gives
//! `bss_money` its exact-fraction API (see [`fraction`]); until then it is the generation under
//! construction.
//!
//! **Vendored.** The external code that runs inside evaluation is a renamed copy under `vendor/`
//! (`bss_money` and the Products derived evaluator, `vendor.toml`); the registry crates they
//! compute with are pinned by version and `Cargo.lock` checksum (`vendor/LOCK.toml`) until the
//! freeze copies them too.
//!
//! **Pure.** No I/O, no clock, no store and no hashing at run time: [`ENGINE_DIGEST`] is computed by
//! the build script over the frozen tree, and the pipeline computes `input_digest`.
#![forbid(unsafe_code)]

// @cpt-dod:cpt-cf-bss-rating-dod-core-evaluate:p1
// @cpt-dod:cpt-cf-bss-rating-dod-core-exact-and-replay:p1
// @cpt-dod:cpt-cf-bss-rating-dod-core-governance:p1
// @cpt-algo:cpt-cf-bss-rating-algo-core-engine-generations:p1

pub mod budget;
pub mod error;
pub mod exact;
pub mod fraction;
pub mod input;
pub mod model;
pub mod outcome;

mod decimal_text;
mod governance;
mod guards;
mod meter;
mod pipeline;
#[cfg(test)]
mod test_input;
mod validate;

pub use budget::ArithmeticBudget;
pub use error::EvaluationError;
pub use input::{EngineId, EvaluationInput};
pub use outcome::EvaluationOutcome;

/// This generation's number, the `engine_generation` results record.
pub const GENERATION: u32 = 1;

/// SHA-256 (lower-case hex) of this generation's frozen tree: its manifest, build script, sources,
/// vendored copies and golden vectors. Computed by the build script; editing any of them changes
/// it.
pub const ENGINE_DIGEST: &str = env!("RATING_ENGINE_GEN_1_DIGEST");

/// The identity results of this generation record: `(engine_generation, engine_digest)`.
#[must_use]
pub fn engine_id() -> EngineId {
    EngineId {
        engine_generation: GENERATION,
        engine_digest: ENGINE_DIGEST.to_owned(),
    }
}

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
