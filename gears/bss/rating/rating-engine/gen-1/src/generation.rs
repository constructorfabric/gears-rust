//! This generation's identity: its number and the digest of its frozen tree (rating ADR-0006).

use crate::budget::ArithmeticBudget;
use crate::error::EvaluationError;
use crate::input::EngineId;

// @cpt-begin:cpt-cf-bss-rating-algo-core-engine-generations:p1:inst-eg-new
/// This generation's number, the `engine_generation` results record. A change of semantics is a
/// new generation crate with the next number, never an edit of a released one.
pub const GENERATION: u32 = 1;
// @cpt-end:cpt-cf-bss-rating-algo-core-engine-generations:p1:inst-eg-new

// @cpt-begin:cpt-cf-bss-rating-algo-core-engine-generations:p1:inst-eg-digest
/// SHA-256 (lower-case hex) of this generation's frozen tree: its manifest, build script, sources,
/// vendored copies, `vendor/LOCK.toml` (which records the money crate's sources and the registry
/// pins) and golden vectors. Computed by the build script; editing any of them changes it.
pub const ENGINE_DIGEST: &str = env!("RATING_ENGINE_GEN_1_DIGEST");
// @cpt-end:cpt-cf-bss-rating-algo-core-engine-generations:p1:inst-eg-digest

/// An input must name this generation, its digest and its budget: another is another engine,
/// never substituted (DESIGN 01 §4.2). The one check `evaluate` and `evaluate_order` share.
///
/// # Errors
///
/// `engine_unavailable` for another generation, digest or budget.
pub fn check(engine: &EngineId, budget: ArithmeticBudget) -> Result<(), EvaluationError> {
    if engine.engine_generation != GENERATION || engine.engine_digest != ENGINE_DIGEST {
        return Err(EvaluationError::EngineUnavailable {
            detail: format!(
                "the input names generation {} with digest {}; this is generation {GENERATION} \
                 with digest {ENGINE_DIGEST}",
                engine.engine_generation, engine.engine_digest
            ),
        });
    }
    if budget != ArithmeticBudget::GEN_1 {
        return Err(EvaluationError::EngineUnavailable {
            detail: format!(
                "the input carries budget {budget:?}; generation {GENERATION} computes under {:?}",
                ArithmeticBudget::GEN_1
            ),
        });
    }
    Ok(())
}

/// The identity results of this generation record: `(engine_generation, engine_digest)`.
#[must_use]
pub fn engine_id() -> EngineId {
    EngineId {
        engine_generation: GENERATION,
        engine_digest: ENGINE_DIGEST.to_owned(),
    }
}
