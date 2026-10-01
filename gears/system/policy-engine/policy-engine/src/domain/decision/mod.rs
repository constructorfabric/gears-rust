//! The decision path of the engine plugin.

pub mod compiled;
pub mod service;

pub use compiled::{CompileCache, CompiledVersion, compile_version};
pub use service::{
    DecisionFailure, DecisionMetrics, DecisionService, NoopDecisionMetrics, Verdict,
};

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod test_support;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod service_tests;
