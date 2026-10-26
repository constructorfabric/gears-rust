//! Typed durable workflows for `ToolKit` gears.
//!
//! A gear embeds an [`Engine`] to run workflows whose progress survives process
//! restarts: sequential steps, fixed parallel branches, runtime fan-out, retries,
//! and saga compensation. Checkpoints are stored in the gear's own database; work
//! is delivered through a PostgreSQL queue. The library does not expose a
//! distributed transaction: external effects run at least once, so activities must
//! deduplicate with [`ActivityContext::idempotency_key`].
//!
//! **Stability:** experimental. The API, the stored format and the error codes may
//! change in any release until the crate is promoted.
//!
//! # Quick start
//!
//! Author a workflow once; the same definition registers handlers and types runs.
//!
//! ```
//! use toolkit_durable_execution::prelude::*;
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
//! # Ok::<(), toolkit_durable_execution::DefinitionError>(())
//! ```
//!
//! Embed the engine in a gear:
//!
//! 1. Add [`migrations()`] to the gear's `DatabaseCapability::migrations`.
//! 2. In `init`, build the engine with [`Engine::builder`] from the gear's `Db` and an
//!    [`Authorizer`]; keep [`Engine::client`] and [`Engine::registry`].
//! 3. Register workflows with [`WorkflowRegistry::register_workflow`], then call
//!    [`Engine::prepare`]; a [`PrepareError`] hands the engine back for a retry.
//! 4. In the gear's lifecycle entry, await [`Runner::run`] with the stop token. Use
//!    [`Engine::shutdown_timeout`] as the stop timeout and [`Engine::readiness`] for
//!    the health check.
//!
//! `examples/consumer_gear.rs` shows the complete wiring. With the `test-util`
//! feature, `testing::TestRuntime` runs workflows in memory, and `testing::RunService`
//! and `testing::InspectService` fake the engine behind [`ExecutionClient`] and
//! [`ExecutionInspector`] in a consumer gear's own tests.
//!
//! # Map
//!
//! | Area | Items |
//! |------|-------|
//! | Lifecycle | [`Engine`], [`EngineBuilder`], [`ExecutionMode`], [`QueueConfig`], [`Runner`], [`Readiness`], [`migrations()`] |
//! | Authoring | [`workflow`], [`authoring`], [`Activity`](workflow::Activity), [`ActivityContext`], [`RetryPolicy`] |
//! | Runs | [`ExecutionClient`], [`RunHandle`], [`StartOptions`], [`observation`] |
//! | Catalog | [`WorkflowRegistry`], [`registration`], [`contracts`] |
//! | Administration | [`ExecutionInspector`] |
//! | Authorization | [`Authorizer`], [`CallerAction`], [`ServiceAction`], [`ConstraintPolicy`] |
//! | Errors | [`Error`], [`ErrorKind`], [`codes`], [`DefinitionError`], [`ConfigError`], [`LifecycleError`], [`PrepareError`] |
//!
//! The library has no dependency on the platform error model. A gear that exposes
//! runs through REST or gRPC maps [`Error::kind`] and [`Error::resource`] to its own
//! canonical errors, and can pass [`Error::code`] as the reason.
//!
//! # Behavior
//!
//! - **Control.** `Cancel`/`Resume` pause and continue forward work. Saga failure or
//!   `Abort` starts reversal in reverse stage and branch order; manual `Retry`
//!   continues failed compensation.
//! - **Authorization.** Every call obtains a fresh scope from the [`Authorizer`];
//!   holding a handle is not a grant. Background work uses the service scope. Run
//!   actions must receive scopes with row constraints; an unconstrained scope for them
//!   is rejected as denied.
//! - **Concurrency.** All handles are `Send + Sync`, cheap to clone and may be used
//!   from any task. Concurrent control commands are serialized by an expected epoch
//!   and revision checks; a lost race returns [`ErrorKind::Aborted`].
//! - **Execution.** [`Runner::run`] spawns workers on the current Tokio runtime and
//!   stops them before it returns. Activities are cancelled through
//!   [`ActivityContext::cancellation`] on shutdown and may run again elsewhere.
//! - **Storage.** Tables use the fixed `durable_execution__` prefix, so a database
//!   or schema hosts one engine. [`migrations()`] records the journal and the
//!   delivery outbox migrations separately in the gear's history. The queue uses the
//!   shared `apalis` schema; every engine needs its own queue names.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

/// Retry policies, failure codes and invocation metadata.
mod activity;
pub mod authoring;
mod authorization;
/// Workflow catalog: persisted definitions, local bindings and registration.
mod catalog;
mod client;
mod config;
pub mod contracts;
/// Queue delivery: the outbox handler and the Apalis adapter.
mod delivery;
mod engine;
mod error;
/// Typed run handles and admission results.
mod handles;
/// Run identities, command options and events.
mod models;
pub mod observation;
/// Persisted catalog generation and lifecycle.
pub mod registration;
/// Type-erased handlers shared by authoring and the executor.
mod runtime;
/// Run state machine: journal transitions, persisted state and views.
mod state;
/// Secure journal persistence and runtime-managed migrations.
mod store;
#[cfg(any(test, feature = "test-util"))]
pub mod testing;
pub mod workflow;

pub use activity::{ActivityContext, ActivityError, ActivityResultExt, ErrorCode, RetryPolicy};
pub use authoring::{Compensation, FanOutOptions};
pub use authorization::{
    AuthorizationError, AuthorizationErrorKind, Authorizer, CallerAction, ConstraintPolicy,
    Resource, ServiceAction,
};
pub use client::{ExecutionClient, ExecutionInspector, RegisteredWorkflow, WorkflowRegistry};
pub use engine::{
    ConfigError, Engine, EngineBuilder, ExecutionMode, LifecycleError, LifecycleErrorKind,
    PrepareError, QueueConfig, Readiness, Runner,
};
pub use error::{DefinitionError, DefinitionErrorKind, Error, ErrorKind, codes};
pub use handles::{RunHandle, StartedRun};
pub use models::{
    AbortOptions, ActivityId, ActivityInstanceId, CancelOptions, CancelResult, ContinueOptions,
    EventKind, ExecutionOwner, ExecutionPhase, RunEvent, RunId, StartOptions, StartResult,
};
pub use store::migrations::migrations;

/// Common authoring and execution items. Contracts, observation and the
/// authorization port live in their own modules.
pub mod prelude {
    pub use crate::workflow::{Activity, Step, StepRef, Workflow, WorkflowBuilder, WorkflowRef};
    pub use crate::{
        AbortOptions, ActivityContext, ActivityError, ActivityResultExt, CancelOptions,
        CancelResult, Compensation, ContinueOptions, ErrorCode, ExecutionClient, FanOutOptions,
        RetryPolicy, RunHandle, RunId, StartOptions, StartedRun, WorkflowRegistry,
    };
}

#[cfg(test)]
#[path = "../tests/unit/authoring_contract_tests.rs"]
mod authoring_contract_tests;

// Lets shared example support code name this crate the same way inside lib tests.
#[cfg(all(test, feature = "integration"))]
extern crate self as toolkit_durable_execution;

/// Helpers shared by unit and integration tests.
#[cfg(test)]
pub(crate) mod test_support {
    use crate::contracts::{DefinitionParts, ExecutionDefinition};
    use crate::{AuthorizationError, Authorizer, CallerAction, ServiceAction};
    use std::sync::Arc;
    use std::time::Duration;
    use toolkit_security::{AccessScope, SecurityContext};

    /// Caller and run-service scopes constrained to one owner tenant; catalog scopes
    /// unconstrained.
    pub struct TenantAuthorizer(pub uuid::Uuid);

    #[async_trait::async_trait]
    impl Authorizer for TenantAuthorizer {
        async fn caller_scope(
            &self,
            _: &SecurityContext,
            _: CallerAction,
        ) -> Result<AccessScope, AuthorizationError> {
            Ok(AccessScope::for_tenant(self.0))
        }
        async fn service_scope(
            &self,
            action: ServiceAction,
        ) -> Result<AccessScope, AuthorizationError> {
            Ok(match action.constraints() {
                crate::ConstraintPolicy::Required => AccessScope::for_tenant(self.0),
                _ => AccessScope::allow_all(),
            })
        }
    }

    /// The engine side of [`TenantAuthorizer`].
    pub fn tenant_authority(tenant: uuid::Uuid) -> crate::authorization::Authority {
        crate::authorization::Authority::new(
            Arc::new(TenantAuthorizer(tenant)),
            Duration::from_secs(5),
        )
    }

    /// Global subscriber that is interested in every callsite and records nothing.
    struct AlwaysInterested;
    impl tracing::Subscriber for AlwaysInterested {
        fn register_callsite(
            &self,
            _: &'static tracing::Metadata<'static>,
        ) -> tracing::subscriber::Interest {
            tracing::subscriber::Interest::always()
        }
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, _: &tracing::Event<'_>) {}
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    /// Keeps every callsite enabled for scoped capturing subscribers.
    ///
    /// Tracing caches callsite interest across all live dispatchers. A test that
    /// registers or rebuilds callsites while no capturing subscriber is alive can
    /// cache "never" after another test's rebuild, so that test's capture silently
    /// sees no events. With an always-interested global default in every snapshot,
    /// interest can no longer become "never"; events still reach only the scoped
    /// subscriber of the emitting task.
    pub fn keep_callsites_enabled() {
        static INSTALLED: std::sync::Once = std::sync::Once::new();
        INSTALLED.call_once(|| {
            // Another global default, if any, keeps callsites registered as well.
            drop(tracing::subscriber::set_global_default(AlwaysInterested));
        });
        tracing::callsite::rebuild_interest_cache();
    }

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

#[cfg(all(test, feature = "integration"))]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../tests/integration/postgres.rs"]
mod test_postgres;

#[cfg(all(test, feature = "integration"))]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../tests/integration/typed_runtime_tests.rs"]
mod typed_runtime_tests;
