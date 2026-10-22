//! Typed persistent workflows. External effects remain at least once.
//!
//! ```
//! use cf_gears_durable_execution_sdk::prelude::*;
//! use std::time::Duration;
//! let builder = WorkflowBuilder::<Vec<u32>>::new("orders.reserve.v1").saga().for_each(
//!     Step::new("reserve", Duration::from_secs(30), |_, item: u32| async move { Ok(item.to_string()) })
//!         .compensate(Compensation::new(Duration::from_secs(30), |_, _: u32, _: String| async { Ok(()) })),
//!     FanOutOptions::default(),
//! );
//! let first = builder.checkpoint().branch(0);
//! let workflow = builder.build()?;
//! let handle = workflow.reference().attach(RunId::from(uuid::Uuid::nil()));
//! assert_eq!(first.fingerprint()?, handle.workflow().fingerprint());
//! # Ok::<(), cf_gears_durable_execution_sdk::DefinitionError>(())
//! ```
//!
//! `Cancel`/`Resume` pause forward work. Saga failure or `Abort` starts reversal;
//! manual `Retry` continues failed compensation. Providers must deduplicate each
//! stable forward/reverse key, including after a lost checkpoint.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
/// Retry policies, failure codes and invocation metadata.
mod activity;
pub mod api;
pub mod authoring;
pub mod contracts;
/// Definition validation errors and canonical error constructors.
mod error;
pub mod gts;
/// Typed run handles and admission results.
mod handles;
/// Run identities, command options and events.
mod models;
pub mod observation;
pub mod reason;
/// Persisted catalog generation and lifecycle.
pub mod registration;
pub mod runtime;
#[cfg(any(test, feature = "test-util"))]
pub mod testing;
pub mod workflow;

#[doc(hidden)]
pub use activity::ActivityContextInit;
pub use activity::{ActivityContext, ActivityError, ActivityResultExt, ErrorCode, RetryPolicy};
pub use api::{
    DurableExecution, DurableExecutionClient, ExecutionInspector, RegisteredWorkflow,
    WorkflowRegistry, WorkflowRegistryExt,
};
pub use authoring::{Compensation, FanOutOptions};
pub use error::{DefinitionError, DefinitionErrorKind};
pub use handles::{RunHandle, StartedRun};
pub use models::{
    AbortOptions, ActivityId, ActivityInstanceId, CancelOptions, CancelResult, ContinueOptions,
    EventKind, ExecutionOwner, ExecutionPhase, RunEvent, RunId, StartOptions, StartResult,
};

/// Common authoring and execution items. Contracts, observation and the runtime
/// boundary live in their own modules.
pub mod prelude {
    pub use crate::workflow::{Activity, Step, StepRef, Workflow, WorkflowBuilder, WorkflowRef};
    pub use crate::{
        AbortOptions, ActivityContext, ActivityError, ActivityResultExt, CancelOptions,
        CancelResult, Compensation, ContinueOptions, DurableExecution, DurableExecutionClient,
        ErrorCode, ExecutionInspector, FanOutOptions, RetryPolicy, RunHandle, RunId, StartOptions,
        StartedRun, WorkflowRegistry, WorkflowRegistryExt,
    };
}

#[cfg(test)]
#[path = "../tests/unit/authoring_contract_tests.rs"]
mod authoring_contract_tests;
