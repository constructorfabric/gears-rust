//! The decision path of the engine plugin.

pub mod builtin;
pub mod compiled;
pub mod service;

pub use builtin::BuiltinPolicySet;
pub use compiled::{CompileCache, CompiledVersion, DocumentCompileError, compile_version};
pub use service::{DecisionMetrics, DecisionService, NoopDecisionMetrics};

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod test_support;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod service_tests;
