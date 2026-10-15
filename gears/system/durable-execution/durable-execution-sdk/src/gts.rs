//! GTS resource-type vocabulary for the durable-execution canonical surface.
//!
//! Each constant MUST equal the literal in the matching impl-crate
//! `#[resource_error]` marker (`infra::error`); the impl crate's error tests
//! pin the equality because the proc-macro cannot reference a const.

use toolkit_gts::gts_id;

/// Tags errors about a durable run. `resource_name` is the run UUID, or the
/// idempotency/coalescing key for a key reused with different input.
pub const RUN_RESOURCE_TYPE: &str = gts_id!("cf.durable_execution.execution.run.v1~");

/// Tags errors about an execution definition. `resource_name` is the
/// versioned definition name.
pub const DEFINITION_RESOURCE_TYPE: &str = gts_id!("cf.durable_execution.execution.definition.v1~");
