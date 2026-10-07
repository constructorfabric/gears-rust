//! Durable execution. Journal checkpoints and fencing are authoritative.
#![forbid(unsafe_code)]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
pub use durable_execution_sdk as sdk;
// Lets shared example support code name this crate the same way inside lib tests.
#[cfg(all(test, feature = "integration"))]
extern crate self as cf_gears_durable_execution;
pub use gear::DurableExecutionGear;
pub mod config;
/// Durable execution domain model: journal, persisted state and errors.
mod domain;
pub mod gear;
/// Helpers shared by unit and integration tests.
#[cfg(test)]
pub(crate) mod test_support {
    use durable_execution_sdk::contracts::{DefinitionParts, ExecutionDefinition};
    /// Rebuild `definition` after `change` edits its parts; panics on an invalid result.
    pub fn edit(
        definition: ExecutionDefinition,
        change: impl FnOnce(&mut DefinitionParts),
    ) -> ExecutionDefinition {
        let mut parts = definition.into_parts();
        change(&mut parts);
        parts.build().expect("edited definition is valid")
    }
}
/// Runtime infrastructure: storage, outbox, workers, authorization and the service.
mod infra;

#[cfg(all(test, feature = "integration"))]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../tests/integration/postgres.rs"]
mod test_postgres;

#[cfg(all(test, feature = "integration"))]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../tests/integration/typed_runtime_tests.rs"]
mod typed_runtime_tests;
