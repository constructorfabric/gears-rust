//! Authorized object-safe capabilities and the typed process-local client.
use crate::contracts::{ExecutionContract, ExecutionDefinition, ResultSelector};
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
use std::{future::Future, sync::Arc, time::Duration};
use toolkit::client_hub::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

/// Run admission, observation and control. Every call obtains fresh authorization;
/// holding a client is not a grant.
#[async_trait]
pub trait DurableExecution: Send + Sync {
    /// Admit or coalesce an owner-scoped run.
    async fn start(
        &self,
        ctx: &SecurityContext,
        definition: &str,
        input: Value,
        options: StartOptions,
    ) -> Result<StartResult, CanonicalError>;
    /// Progress and its event cursor, read from one consistent journal revision.
    async fn get(&self, ctx: &SecurityContext, id: RunId) -> Result<RunProgress, CanonicalError>;
    /// Payload-free notifications committed after the `after` cursor.
    async fn events(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        after: u64,
        limit: u32,
    ) -> Result<Vec<RunEvent>, CanonicalError>;
    /// Payload-free attempt and execution-epoch history.
    async fn history(&self, ctx: &SecurityContext, id: RunId)
    -> Result<RunHistory, CanonicalError>;
    /// Serialized workflow result or checkpoint. Requires owner authorization,
    /// independent of administrative inspection.
    async fn result(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        definition: &str,
        expected_fingerprint: &str,
        selector: ResultSelector,
    ) -> Result<Value, CanonicalError>;
    /// Definitively stop forward work; sagas reverse their confirmed effects.
    async fn abort(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        options: AbortOptions,
    ) -> Result<CancelResult, CanonicalError>;
    /// Pause forward work at the caller-observed epoch.
    async fn cancel(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        options: CancelOptions,
    ) -> Result<CancelResult, CanonicalError>;
    /// Continue a failed run, or failed compensation, from its saved checkpoints.
    async fn retry(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        options: ContinueOptions,
    ) -> Result<RunProgress, CanonicalError>;
    /// Continue paused forward work from its saved checkpoints.
    async fn resume(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        options: ContinueOptions,
    ) -> Result<RunProgress, CanonicalError>;
}

/// Trusted definition management; never expose through user-facing REST or gRPC.
#[async_trait]
pub trait WorkflowRegistry: Send + Sync {
    /// Persist a serializable contract without worker handlers.
    async fn register_contract(
        &self,
        contract: ExecutionContract,
    ) -> Result<Registration, CanonicalError>;
    /// Validate, persist and bind a local workflow definition.
    async fn register(
        &self,
        definition: ExecutionDefinition,
    ) -> Result<Registration, CanonicalError>;
    /// Current persisted catalog generation and lifecycle.
    async fn registration(&self, name: &str) -> Result<Registration, CanonicalError>;
    /// Retire admission, or drain work before releasing bindings.
    async fn unregister(
        &self,
        name: &str,
        options: UnregisterOptions,
    ) -> Result<Registration, CanonicalError>;
    /// Reactivate an eligible persisted definition at its expected revision.
    async fn activate(
        &self,
        name: &str,
        expected_revision: u64,
    ) -> Result<Registration, CanonicalError>;
}

/// A registered workflow: its typed reference and catalog state.
#[derive(Debug)]
pub struct RegisteredWorkflow<I, O> {
    /// Typed contract identity used to start runs and read results.
    pub reference: WorkflowRef<I, O>,
    /// Catalog generation and revision after registration.
    pub registration: Registration,
}

/// Typed registration on top of [`WorkflowRegistry`].
pub trait WorkflowRegistryExt: WorkflowRegistry {
    /// Register `workflow` and keep its typed reference.
    ///
    /// # Errors
    /// Propagates validation, conflict and storage errors of [`WorkflowRegistry::register`].
    fn register_workflow<I, O>(
        &self,
        workflow: Workflow<I, O>,
    ) -> impl Future<Output = Result<RegisteredWorkflow<I, O>, CanonicalError>> + Send;
}
impl<T: WorkflowRegistry + ?Sized> WorkflowRegistryExt for T {
    async fn register_workflow<I, O>(
        &self,
        workflow: Workflow<I, O>,
    ) -> Result<RegisteredWorkflow<I, O>, CanonicalError> {
        let (reference, definition) = workflow.into_parts();
        let registration = self.register(definition).await?;
        Ok(RegisteredWorkflow {
            reference,
            registration,
        })
    }
}

/// Administrative progress only. No implicit access to checkpoint payloads.
#[async_trait]
pub trait ExecutionInspector: Send + Sync {
    /// Separately authorized administrative progress without payload access.
    async fn inspect(
        &self,
        ctx: &SecurityContext,
        id: RunId,
    ) -> Result<RunProgress, CanonicalError>;
    /// A scoped page of administrative progress projections.
    async fn list(
        &self,
        ctx: &SecurityContext,
        query: WorkflowQuery,
    ) -> Result<WorkflowPage, CanonicalError>;
}

/// Typed client over the object-safe [`DurableExecution`] service.
#[derive(Clone)]
pub struct DurableExecutionClient {
    inner: Arc<dyn DurableExecution>,
    poll_interval: Duration,
}
impl DurableExecutionClient {
    /// Default delay between progress reads in [`Self::wait_until`].
    pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(100);

    /// Wrap an object-safe service.
    #[must_use]
    pub fn new(inner: Arc<dyn DurableExecution>) -> Self {
        Self {
            inner,
            poll_interval: Self::DEFAULT_POLL_INTERVAL,
        }
    }
    /// Resolve the service published by the host.
    ///
    /// # Errors
    /// `ServiceUnavailable` when the host has not published the capability.
    pub fn resolve(hub: &ClientHub) -> Result<Self, CanonicalError> {
        hub.get::<dyn DurableExecution>()
            .map(Self::new)
            .map_err(|_| {
                CanonicalError::service_unavailable()
                    .with_detail("durable execution client is not registered")
                    .create()
            })
    }
    /// Use `interval` between progress reads in [`Self::wait_until`].
    #[must_use]
    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }
    /// The object-safe service, for operations on serialized contracts.
    #[must_use]
    pub fn raw(&self) -> &dyn DurableExecution {
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
    ) -> Result<StartedRun<I, O>, CanonicalError> {
        let fingerprint = workflow.fingerprint();
        if options
            .expected_fingerprint
            .as_deref()
            .is_some_and(|expected| expected != fingerprint)
        {
            return Err(crate::error::invalid_input(
                "expected_fingerprint",
                "fingerprint differs from the workflow reference",
            ));
        }
        options.expected_fingerprint = Some(fingerprint.to_owned());
        let input = serde_json::to_value(input).map_err(|_| {
            crate::error::invalid_input("input", "workflow input cannot be serialized")
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
    pub async fn get(
        &self,
        ctx: &SecurityContext,
        id: RunId,
    ) -> Result<RunProgress, CanonicalError> {
        self.inner.get(ctx, id).await
    }

    /// Read progress until the run [is settled](crate::observation::RunState::is_settled):
    /// terminal, failed compensation or blocked.
    ///
    /// Waits without bound; wrap it in `tokio::time::timeout` to limit it.
    ///
    /// # Errors
    /// Propagates the first failed progress read.
    pub async fn wait(
        &self,
        ctx: &SecurityContext,
        id: RunId,
    ) -> Result<RunProgress, CanonicalError> {
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
    ) -> Result<RunProgress, CanonicalError> {
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
    /// # use cf_gears_durable_execution_sdk::{DurableExecutionClient, RunId};
    /// # async fn demo(client: DurableExecutionClient, ctx: toolkit_security::SecurityContext, id: RunId) {
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
    ) -> impl Stream<Item = Result<RunProgress, CanonicalError>> + Send + 'a {
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
    ) -> Result<O, CanonicalError> {
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
        decode(value)
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
    ) -> Result<R::Output, CanonicalError> {
        let step = reference.as_step_ref();
        let workflow = handle.workflow();
        if step.workflow_name() != workflow.name()
            || step.fingerprint().map_err(CanonicalError::from)? != workflow.fingerprint()
        {
            return Err(crate::error::definition_mismatch());
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
        decode(value)
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
    ) -> Result<Vec<RunEvent>, CanonicalError> {
        self.inner.events(ctx, id, after, limit).await
    }
    /// Attempt and epoch history.
    ///
    /// # Errors
    /// Propagates authorization, missing-run and storage errors.
    pub async fn history(
        &self,
        ctx: &SecurityContext,
        id: RunId,
    ) -> Result<RunHistory, CanonicalError> {
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
    ) -> Result<CancelResult, CanonicalError> {
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
    ) -> Result<CancelResult, CanonicalError> {
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
    ) -> Result<RunProgress, CanonicalError> {
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
    ) -> Result<RunProgress, CanonicalError> {
        self.inner.resume(ctx, id, options).await
    }
}
/// Position of [`DurableExecutionClient::watch`].
enum Watch {
    Start,
    After(u64),
    Done,
}
fn decode<T: DeserializeOwned>(value: Value) -> Result<T, CanonicalError> {
    serde_json::from_value(value).map_err(|_| {
        CanonicalError::internal("saved workflow result does not match the requested payload type")
            .create()
    })
}
impl std::fmt::Debug for DurableExecutionClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DurableExecutionClient")
            .field("poll_interval", &self.poll_interval)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../tests/unit/api_tests.rs"]
mod tests;
