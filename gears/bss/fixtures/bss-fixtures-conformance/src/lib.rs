//! Conformance harness over the joint fixture corpus.
//!
//! Carries the `ChargeEvaluator` trait, the suite runner, and the reference
//! oracle. Gears take this as a **dev-dependency only**.

pub mod oracle;
pub mod registry_gen;
pub mod runner;
pub mod traits;

pub use oracle::ReferenceOracle;
pub use runner::{
    Outcome, PublishOutcome, PublishReport, Report, run_evaluation_suite, run_publish_suite,
};
pub use traits::{CorpusEvaluator, EvalError, EvalInput, Evaluated, PublishValidator};
