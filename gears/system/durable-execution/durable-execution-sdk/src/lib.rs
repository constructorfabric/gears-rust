//! Typed persistent workflows. External effects remain at least once.
#![forbid(unsafe_code)]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
mod activity;
pub mod api;
pub mod contracts;
mod error;
pub mod gts;
mod models;
pub mod observation;
pub mod reason;
pub mod registration;
pub mod workflow;
pub use activity::{ActivityContext, ActivityError, RetryPolicy};
pub use api::{DurableExecution, DurableExecutionClient, ExecutionInspector, WorkflowRegistry};
pub use error::DefinitionError;
pub use models::{
    ActivityId, CancelOptions, CancelResult, ContinueOptions, EventKind, ExecutionOwner, RunEvent,
    RunId, StartOptions, StartResult,
};
/// Common authoring and execution types. Advanced contracts and observation are separate modules.
pub mod prelude {
    pub use crate::workflow::{
        Activity, Step, StepContext, StepRef, Workflow, WorkflowBuilder, WorkflowRef,
    };
    pub use crate::{
        ActivityContext, ActivityError, CancelOptions, CancelResult, ContinueOptions,
        DurableExecution, DurableExecutionClient, ExecutionInspector, RetryPolicy, RunId,
        StartOptions, WorkflowRegistry,
    };
}
