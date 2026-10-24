//! BSS Rating evaluation core (DESIGN §1.3, ADR-0002).
//!
//! The deterministic, I/O-free half of Rating: the evaluation steps in their fixed order (bind,
//! meter, model, floor, guards), the engine generations and the exact arithmetic over
//! `ExactMoney` and `ExactQuantity`.
//!
//! **Engine generations** (DESIGN §4.1, ADR-0006). Every change of evaluation semantics is a
//! frozen generation crate, re-exported here as [`engine`]`::gen_<n>`, and every generation a
//! retained result references stays compiled in. [`EngineRegistry`] dispatches by the
//! `(engine_generation, engine_digest)` an input names and never substitutes another generation:
//! a missing one, or one whose digest differs, is `engine_unavailable`.
//!
//! **Types.** The input, outcome and error types of this crate are those of the current
//! generation ([`engine::current`]): it is what the pipeline assembles new work with. When a later
//! generation becomes current, the engines of the older ones map its types into their own through
//! their canonical JSON (numbers as canonical text), and a value an older generation does not
//! know is its `unknown_enum_value`; no workspace type crosses into a generation.
//!
//! **Purity.** No tokio, sea-orm, http or `ClientHub` may enter this crate's normal dependency tree,
//! directly or transitively: an evaluation must give a byte-identical result when it is replayed
//! years later, and nothing that reads a clock, a database or the network may take part in it. CI
//! checks the tree with `make check-pure-crates` (`tools/scripts/pure_crates.toml`). This is also
//! why the core does not depend on `bss-pricing-sdk`, which pulls `http` through
//! `toolkit-contract`: the pricing adapter in the `rating` gear maps Pricing's types into the
//! core's own frozen input types.
#![forbid(unsafe_code)]

// @cpt-dod:cpt-cf-bss-rating-dod-core-evaluate:p1
// @cpt-dod:cpt-cf-bss-rating-dod-core-exact-and-replay:p1

pub mod canonical;
pub mod engine;
mod registry;

pub use engine::current::{
    ArithmeticBudget, EngineId, EvaluationError, EvaluationInput, EvaluationOutcome,
};
pub use registry::{Engine, EngineRegistry, Gen1, RegistryError};
