//! Handles returned by [`Engine`](crate::Engine): the typed run client, the trusted
//! workflow registry and the administrative inspector.
//!
//! All handles are cheap to clone and safe to share between tasks. Holding a
//! handle is not a grant: every call obtains fresh authorization.
use crate::Error;
use crate::catalog::registrar::Registrar;
use crate::contracts::{ExecutionContract, ResultSelector};
use crate::observation::{RunHistory, RunProgress, WorkflowPage, WorkflowQuery};
use crate::registration::{Registration, UnregisterOptions};
use crate::workflow::{CheckpointReference, Payload, Workflow, WorkflowRef};
use crate::{
    AbortOptions, CancelOptions, CancelResult, ContinueOptions, RunEvent, RunHandle, RunId,
    StartOptions, StartResult, StartedRun,
};
use async_trait::async_trait;
use futures_util::Stream;
use serde::de::DeserializeOwned;
use serde_json::Value;
use service::Service;
use std::{sync::Arc, time::Duration};
use toolkit_security::SecurityContext;

/// Caller-scoped service behind the run client and the inspector.
pub mod service;

/// Run admission, observation and control behind [`ExecutionClient`].
///
/// The engine supplies the storage-backed implementation. With the `test-util`
/// feature, tests of a consumer gear implement it to fake the engine and wrap it
/// with `ExecutionClient::from_service`.
#[async_trait]
pub trait RunService: Send + Sync {
    /// Admit or coalesce an owner-scoped run.
    async fn start(
        &self,
        ctx: &SecurityContext,
        definition: &str,
        input: Value,
        options: StartOptions,
    ) -> Result<StartResult, Error>;
    /// Progress and its event cursor, read from one consistent journal revision.
    async fn get(&self, ctx: &SecurityContext, id: RunId) -> Result<RunProgress, Error>;
    /// Payload-free notifications committed after the `after` cursor.
    async fn events(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        after: u64,
        limit: u32,
    ) -> Result<Vec<RunEvent>, Error>;
    /// Payload-free attempt and execution-epoch history.
    async fn history(&self, ctx: &SecurityContext, id: RunId) -> Result<RunHistory, Error>;
    /// Serialized workflow result or checkpoint. Requires owner authorization,
    /// independent of administrative inspection.
    async fn result(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        definition: &str,
        expected_fingerprint: &str,
        selector: ResultSelector,
    ) -> Result<Value, Error>;
    /// Definitively stop forward work; sagas reverse their confirmed effects.
    async fn abort(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        options: AbortOptions,
    ) -> Result<CancelResult, Error>;
    /// Pause forward work at the caller-observed epoch.
    async fn cancel(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        options: CancelOptions,
    ) -> Result<CancelResult, Error>;
    /// Continue a failed run, or failed compensation, from its saved checkpoints.
    async fn retry(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        options: ContinueOptions,
    ) -> Result<RunProgress, Error>;
    /// Continue paused forward work from its saved checkpoints.
    async fn resume(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        options: ContinueOptions,
    ) -> Result<RunProgress, Error>;
}

/// Trusted definition management. Never expose it through user-facing REST or gRPC.
///
/// Calls run under the service identity
/// ([`Authorizer::service_scope`](crate::Authorizer::service_scope)).
#[derive(Clone)]
pub struct WorkflowRegistry {
    inner: Arc<Registrar>,
}

impl WorkflowRegistry {
    /// Registry over the engine's registrar.
    pub(crate) fn new(inner: Arc<Registrar>) -> Self {
        Self { inner }
    }

    /// Validate, persist and bind `workflow`, keeping its typed reference.
    ///
    /// Each process binds a workflow once. Other processes register the same
    /// contract and bind their own handlers; the catalog entry is shared.
    /// Registration may happen before or after
    /// [`Engine::prepare`](crate::Engine::prepare); runs wait for their handlers.
    ///
    /// # Errors
    /// `AlreadyExists` for a different contract under the same name, or for a second
    /// binding of the same name in this process; `PermissionDenied`/`Unavailable`
    /// from service authorization,
    /// `Unavailable` for storage failures.
    pub async fn register_workflow<I, O>(
        &self,
        workflow: Workflow<I, O>,
    ) -> Result<RegisteredWorkflow<I, O>, Error> {
        let (reference, definition) = workflow.into_parts();
        let registration = self.inner.register(definition).await?;
        Ok(RegisteredWorkflow {
            reference,
            registration,
        })
    }

    /// Persist a serializable contract without local handlers, for hosts that only
    /// admit and observe runs executed elsewhere.
    ///
    /// # Errors
    /// Same as [`Self::register_workflow`].
    pub async fn register_contract(
        &self,
        contract: ExecutionContract,
    ) -> Result<Registration, Error> {
        self.inner.register_contract(contract).await
    }

    /// Current persisted catalog generation and lifecycle.
    ///
    /// # Errors
    /// `NotFound` for an unknown name, authorization and storage errors.
    pub async fn registration(&self, name: &str) -> Result<Registration, Error> {
        self.inner.registration(name).await
    }

    /// Retire admission, or drain work before releasing bindings.
    ///
    /// # Errors
    /// `FailedPrecondition` for a state that forbids the transition, `Aborted` for a
    /// concurrent catalog update, authorization and storage errors.
    pub async fn unregister(
        &self,
        name: &str,
        options: UnregisterOptions,
    ) -> Result<Registration, Error> {
        self.inner.unregister(name, options).await
    }

    /// Reactivate an eligible persisted definition at its expected revision.
    ///
    /// # Errors
    /// Same as [`Self::unregister`].
    pub async fn activate(
        &self,
        name: &str,
        expected_revision: u64,
    ) -> Result<Registration, Error> {
        self.inner.activate(name, expected_revision).await
    }
}

impl std::fmt::Debug for WorkflowRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowRegistry").finish_non_exhaustive()
    }
}

/// A registered workflow: its typed reference and catalog state.
#[derive(Debug)]
pub struct RegisteredWorkflow<I, O> {
    /// Typed contract identity used to start runs and read results.
    pub reference: WorkflowRef<I, O>,
    /// Catalog generation and revision after registration.
    pub registration: Registration,
}

/// Administrative reads behind [`ExecutionInspector`].
///
/// The engine supplies the storage-backed implementation. With the `test-util`
/// feature, tests of a consumer gear implement it to fake the engine and wrap it
/// with `ExecutionInspector::from_service`.
#[async_trait]
pub trait InspectService: Send + Sync {
    /// Administrative progress of one run.
    async fn inspect(&self, ctx: &SecurityContext, id: RunId) -> Result<RunProgress, Error>;
    /// A scoped page of administrative progress projections.
    async fn list(
        &self,
        ctx: &SecurityContext,
        query: WorkflowQuery,
    ) -> Result<WorkflowPage, Error>;
}

#[async_trait]
impl InspectService for Service {
    async fn inspect(&self, ctx: &SecurityContext, id: RunId) -> Result<RunProgress, Error> {
        Service::inspect(self, ctx, id).await
    }
    async fn list(
        &self,
        ctx: &SecurityContext,
        query: WorkflowQuery,
    ) -> Result<WorkflowPage, Error> {
        Service::list(self, ctx, query).await
    }
}

/// Administrative progress reads. Grants no access to inputs, results or checkpoints.
///
/// Authorized with [`CallerAction::Inspect`](crate::CallerAction::Inspect) and
/// [`CallerAction::List`](crate::CallerAction::List), independent of run ownership.
#[derive(Clone)]
pub struct ExecutionInspector {
    inner: Arc<dyn InspectService>,
}

impl ExecutionInspector {
    /// Inspector over the engine's run service.
    pub(crate) fn new(inner: Arc<dyn InspectService>) -> Self {
        Self { inner }
    }

    /// An inspector over a fake service, for tests of a consumer gear.
    #[cfg(any(test, feature = "test-util"))]
    #[must_use]
    pub fn from_service(service: Arc<dyn InspectService>) -> Self {
        Self::new(service)
    }

    /// Progress of any run in the caller's scope.
    ///
    /// # Errors
    /// `NotFound` outside the scope, authorization and storage errors.
    pub async fn inspect(&self, ctx: &SecurityContext, id: RunId) -> Result<RunProgress, Error> {
        self.inner.inspect(ctx, id).await
    }

    /// A scoped page of progress projections.
    ///
    /// # Errors
    /// `InvalidArgument` for a malformed query, authorization and storage errors.
    pub async fn list(
        &self,
        ctx: &SecurityContext,
        query: WorkflowQuery,
    ) -> Result<WorkflowPage, Error> {
        self.inner.list(ctx, query).await
    }
}

impl std::fmt::Debug for ExecutionInspector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutionInspector").finish_non_exhaustive()
    }
}

/// Typed client for starting, observing and controlling runs.
///
/// Every call authorizes the given [`SecurityContext`] with
/// [`Authorizer::caller_scope`](crate::Authorizer::caller_scope). Runs are visible
/// only to their owner unless a method says otherwise.
#[derive(Clone)]
pub struct ExecutionClient {
    inner: Arc<dyn RunService>,
    poll_interval: Duration,
}
impl ExecutionClient {
    /// Default delay between progress reads in [`Self::wait_until`].
    pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(100);

    /// Client over the engine's run service, polling at
    /// [`DEFAULT_POLL_INTERVAL`](Self::DEFAULT_POLL_INTERVAL).
    pub(crate) fn new(inner: Arc<dyn RunService>) -> Self {
        Self {
            inner,
            poll_interval: Self::DEFAULT_POLL_INTERVAL,
        }
    }
    /// A client over a fake service, for tests of a consumer gear.
    #[cfg(any(test, feature = "test-util"))]
    #[must_use]
    pub fn from_service(service: Arc<dyn RunService>) -> Self {
        Self::new(service)
    }
    /// Use `interval` between progress reads in [`Self::wait_until`].
    #[must_use]
    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }
    /// The untyped service, for tests on serialized contracts.
    #[cfg(test)]
    pub(crate) fn raw(&self) -> &dyn RunService {
        self.inner.as_ref()
    }

    /// Start a run of `workflow`, guarded by its contract fingerprint.
    ///
    /// # Errors
    /// `InvalidArgument` for an unserializable input or a conflicting
    /// `expected_fingerprint`, otherwise the authorized admission error.
    pub async fn start<I: Payload, O>(
        &self,
        ctx: &SecurityContext,
        workflow: &WorkflowRef<I, O>,
        input: I,
        mut options: StartOptions,
    ) -> Result<StartedRun<I, O>, Error> {
        let fingerprint = workflow.fingerprint();
        if options
            .expected_fingerprint
            .as_deref()
            .is_some_and(|expected| expected != fingerprint)
        {
            return Err(Error::invalid_input(
                "expected_fingerprint",
                "fingerprint differs from the workflow reference",
            ));
        }
        options.expected_fingerprint = Some(fingerprint.to_owned());
        let input = serde_json::to_value(input).map_err(|cause| {
            Error::invalid_input("input", "workflow input cannot be serialized").with_source(cause)
        })?;
        let result = self
            .inner
            .start(ctx, workflow.name(), input, options)
            .await?;
        Ok(StartedRun::new(result, workflow))
    }

    /// Current progress.
    ///
    /// # Errors
    /// Propagates authorization, missing-run and storage errors.
    pub async fn get(&self, ctx: &SecurityContext, id: RunId) -> Result<RunProgress, Error> {
        self.inner.get(ctx, id).await
    }

    /// Read progress until the run [is settled](crate::observation::RunState::is_settled):
    /// terminal, failed compensation or blocked.
    ///
    /// Waits without bound; wrap it in `tokio::time::timeout` to limit it.
    ///
    /// # Errors
    /// Propagates the first failed progress read.
    pub async fn wait(&self, ctx: &SecurityContext, id: RunId) -> Result<RunProgress, Error> {
        self.wait_until(ctx, id, |progress| progress.state.is_settled())
            .await
    }

    /// Read progress every poll interval until `done` accepts it.
    ///
    /// Waits without bound; wrap it in `tokio::time::timeout` to limit it.
    ///
    /// # Errors
    /// Propagates the first failed progress read.
    pub async fn wait_until(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        mut done: impl FnMut(&RunProgress) -> bool + Send,
    ) -> Result<RunProgress, Error> {
        loop {
            let progress = self.inner.get(ctx, id).await?;
            if done(&progress) {
                return Ok(progress);
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    /// Stream progress whenever the run commits new events, ending after a
    /// [settled](crate::observation::RunState::is_settled) state or the first error.
    ///
    /// The first item is the current progress. Afterwards the stream reads events after
    /// the last cursor every poll interval and re-reads progress only when one arrived,
    /// as the event contract intends.
    ///
    /// ```no_run
    /// # use toolkit_durable_execution::{ExecutionClient, RunId};
    /// # async fn demo(client: ExecutionClient, ctx: toolkit_security::SecurityContext, id: RunId) {
    /// use futures_util::StreamExt;
    /// let mut updates = std::pin::pin!(client.watch(&ctx, id));
    /// while let Some(progress) = updates.next().await {
    ///     let _status = progress.map(|p| p.state.status());
    /// }
    /// # }
    /// ```
    pub fn watch<'a>(
        &'a self,
        ctx: &'a SecurityContext,
        id: RunId,
    ) -> impl Stream<Item = Result<RunProgress, Error>> + Send + 'a {
        futures_util::stream::unfold(Watch::Start, move |state| async move {
            let after = match state {
                Watch::Done => return None,
                Watch::Start => None,
                Watch::After(cursor) => Some(cursor),
            };
            if let Some(after) = after {
                loop {
                    match self.inner.events(ctx, id, after, 1).await {
                        Ok(events) if events.is_empty() => {
                            tokio::time::sleep(self.poll_interval).await;
                        }
                        Ok(_) => break,
                        Err(error) => return Some((Err(error), Watch::Done)),
                    }
                }
            }
            match self.inner.get(ctx, id).await {
                Ok(progress) => {
                    let next = if progress.state.is_settled() {
                        Watch::Done
                    } else {
                        Watch::After(progress.cursor)
                    };
                    Some((Ok(progress), next))
                }
                Err(error) => Some((Err(error), Watch::Done)),
            }
        })
    }

    /// The successful workflow result.
    ///
    /// # Errors
    /// `FailedPrecondition` when the run has no successful result or a different
    /// contract; `Internal` when the saved payload does not decode as `O`.
    pub async fn result<I, O: Payload>(
        &self,
        ctx: &SecurityContext,
        handle: &RunHandle<I, O>,
    ) -> Result<O, Error> {
        let workflow = handle.workflow();
        let value = self
            .inner
            .result(
                ctx,
                handle.id(),
                workflow.name(),
                workflow.fingerprint(),
                ResultSelector::Workflow,
            )
            .await?;
        decode(value, false)
    }

    /// A confirmed checkpoint of the run.
    ///
    /// # Errors
    /// `FailedPrecondition` when `reference` belongs to another contract or the
    /// checkpoint is absent; `Internal` when the payload does not decode.
    pub async fn step_result<I, O, R: CheckpointReference>(
        &self,
        ctx: &SecurityContext,
        handle: &RunHandle<I, O>,
        reference: &R,
    ) -> Result<R::Output, Error> {
        let step = reference.as_step_ref();
        let workflow = handle.workflow();
        if step.workflow_name() != workflow.name()
            || step.fingerprint().map_err(Error::from)? != workflow.fingerprint()
        {
            return Err(crate::error::domain::DomainError::DefinitionMismatch.into());
        }
        let value = self
            .inner
            .result(
                ctx,
                handle.id(),
                workflow.name(),
                workflow.fingerprint(),
                ResultSelector::Checkpoint(step.source().clone()),
            )
            .await?;
        decode(value, true)
    }

    /// Notifications committed after the `after` cursor; re-read progress after one.
    ///
    /// # Errors
    /// Propagates authorization, cursor validation and storage errors.
    pub async fn events(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        after: u64,
        limit: u32,
    ) -> Result<Vec<RunEvent>, Error> {
        self.inner.events(ctx, id, after, limit).await
    }
    /// Attempt and epoch history.
    ///
    /// # Errors
    /// Propagates authorization, missing-run and storage errors.
    pub async fn history(&self, ctx: &SecurityContext, id: RunId) -> Result<RunHistory, Error> {
        self.inner.history(ctx, id).await
    }
    /// Definitively stop forward work; sagas reverse their confirmed effects.
    ///
    /// # Errors
    /// Propagates authorization, stale-epoch and transition errors.
    pub async fn abort(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        options: AbortOptions,
    ) -> Result<CancelResult, Error> {
        self.inner.abort(ctx, id, options).await
    }
    /// Pause forward work.
    ///
    /// # Errors
    /// Propagates authorization, stale-epoch and transition errors.
    pub async fn cancel(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        options: CancelOptions,
    ) -> Result<CancelResult, Error> {
        self.inner.cancel(ctx, id, options).await
    }
    /// Continue a failed run, or failed compensation, from its saved checkpoints.
    ///
    /// # Errors
    /// Propagates authorization, stale-epoch, contract and transition errors.
    pub async fn retry(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        options: ContinueOptions,
    ) -> Result<RunProgress, Error> {
        self.inner.retry(ctx, id, options).await
    }
    /// Continue paused forward work from its saved checkpoints.
    ///
    /// # Errors
    /// Propagates authorization, stale-epoch, contract and transition errors.
    pub async fn resume(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        options: ContinueOptions,
    ) -> Result<RunProgress, Error> {
        self.inner.resume(ctx, id, options).await
    }
}
/// Position of [`ExecutionClient::watch`].
enum Watch {
    Start,
    After(u64),
    Done,
}
fn decode<T: DeserializeOwned>(value: Value, checkpoint: bool) -> Result<T, Error> {
    serde_json::from_value(value).map_err(|cause| Error::undecodable(checkpoint, cause))
}
impl std::fmt::Debug for ExecutionClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutionClient")
            .field("poll_interval", &self.poll_interval)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../tests/unit/client_tests.rs"]
mod tests;
