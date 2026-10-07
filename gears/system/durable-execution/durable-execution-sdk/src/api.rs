//! Authorized object-safe capabilities and the typed process-local client.
use crate::contracts::{ExecutionContract, ExecutionDefinition, InputSource};
use crate::observation::{RunHistory, RunProgress, WorkflowPage, WorkflowQuery};
use crate::registration::{Registration, UnregisterOptions};
use crate::workflow::{Payload, StepRef, WorkflowRef};
use crate::{
    CancelOptions, CancelResult, ContinueOptions, RunEvent, RunId, StartOptions, StartResult,
};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use toolkit::client_hub::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

/// Every call obtains fresh authorization; an embedded client is not a grant.
#[async_trait]
pub trait DurableExecution: Send + Sync {
    async fn start(
        &self,
        ctx: &SecurityContext,
        definition: &str,
        input: Value,
        options: StartOptions,
    ) -> Result<StartResult, CanonicalError>;
    /// Progress and event cursor are read from one consistent journal revision.
    async fn get(&self, ctx: &SecurityContext, id: RunId) -> Result<RunProgress, CanonicalError>;
    async fn events(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        after: u64,
        limit: u32,
    ) -> Result<Vec<RunEvent>, CanonicalError>;
    async fn history(&self, ctx: &SecurityContext, id: RunId)
    -> Result<RunHistory, CanonicalError>;
    /// Results require owner authorization independently of administrative inspection.
    async fn result(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        definition: &str,
        source: Option<InputSource>,
    ) -> Result<Value, CanonicalError>;
    async fn cancel(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        options: CancelOptions,
    ) -> Result<CancelResult, CanonicalError>;
    async fn retry(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        options: ContinueOptions,
    ) -> Result<RunProgress, CanonicalError>;
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
    async fn register_contract(
        &self,
        contract: ExecutionContract,
    ) -> Result<Registration, CanonicalError>;
    async fn register(
        &self,
        definition: ExecutionDefinition,
    ) -> Result<Registration, CanonicalError>;
    async fn registration(&self, name: &str) -> Result<Registration, CanonicalError>;
    async fn unregister(
        &self,
        name: &str,
        options: UnregisterOptions,
    ) -> Result<Registration, CanonicalError>;
    async fn activate(
        &self,
        name: &str,
        expected_revision: u64,
    ) -> Result<Registration, CanonicalError>;
}
/// Administrative progress only. No implicit access to checkpoint payloads.
#[async_trait]
pub trait ExecutionInspector: Send + Sync {
    async fn inspect(
        &self,
        ctx: &SecurityContext,
        id: RunId,
    ) -> Result<RunProgress, CanonicalError>;
    async fn list(
        &self,
        ctx: &SecurityContext,
        query: WorkflowQuery,
    ) -> Result<WorkflowPage, CanonicalError>;
}
#[derive(Clone)]
pub struct DurableExecutionClient {
    inner: Arc<dyn DurableExecution>,
}
impl DurableExecutionClient {
    pub fn new(inner: Arc<dyn DurableExecution>) -> Self {
        Self { inner }
    }
    /// # Errors
    /// Returns `ServiceUnavailable` when the host has not published the capability.
    pub fn resolve(hub: &ClientHub) -> Result<Self, CanonicalError> {
        hub.get::<dyn DurableExecution>()
            .map(Self::new)
            .map_err(|_| {
                CanonicalError::service_unavailable()
                    .with_detail("durable execution client is not registered")
                    .create()
            })
    }
    #[must_use]
    pub fn raw(&self) -> &dyn DurableExecution {
        self.inner.as_ref()
    }
    /// # Errors
    /// Returns `InvalidArgument` for incompatible payloads or pins, or the authorized admission error.
    pub async fn start<I: Payload, O>(
        &self,
        ctx: &SecurityContext,
        workflow: &WorkflowRef<I, O>,
        input: I,
        mut options: StartOptions,
    ) -> Result<StartResult, CanonicalError> {
        let fingerprint = workflow
            .contract()
            .fingerprint()
            .map_err(CanonicalError::from)?;
        if options
            .expected_fingerprint
            .as_ref()
            .is_some_and(|expected| *expected != fingerprint)
        {
            return Err(crate::error::invalid_input(
                "expected_fingerprint",
                "fingerprint differs from the workflow reference",
            ));
        }
        options.expected_fingerprint = Some(fingerprint);
        let input = serde_json::to_value(input).map_err(|_| {
            crate::error::invalid_input("input", "workflow input cannot be serialized")
        })?;
        self.inner.start(ctx, workflow.name(), input, options).await
    }
    /// # Errors
    /// Propagates authorization, missing-run and storage errors.
    pub async fn get(
        &self,
        ctx: &SecurityContext,
        id: RunId,
    ) -> Result<RunProgress, CanonicalError> {
        self.inner.get(ctx, id).await
    }
    /// # Errors
    /// Propagates result-read errors; returns Internal if the saved payload cannot decode as the requested type.
    pub async fn result<I, O: Payload>(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        workflow: &WorkflowRef<I, O>,
    ) -> Result<O, CanonicalError> {
        let value = self.inner.result(ctx, id, workflow.name(), None).await?;
        serde_json::from_value(value).map_err(|_| {
            CanonicalError::internal(
                "saved workflow result does not match the requested payload type",
            )
            .create()
        })
    }
    /// # Errors
    /// Propagates checkpoint-read errors; returns Internal if the checkpoint cannot decode as the requested type.
    pub async fn step_result<T: Payload>(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        step: &StepRef<T>,
    ) -> Result<T, CanonicalError> {
        let value = self
            .inner
            .result(ctx, id, step.workflow_name(), Some(step.source().clone()))
            .await?;
        serde_json::from_value(value).map_err(|_| {
            CanonicalError::internal(
                "saved workflow result does not match the requested payload type",
            )
            .create()
        })
    }
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
    /// # Errors
    /// Propagates authorization, missing-run and storage errors.
    pub async fn history(
        &self,
        ctx: &SecurityContext,
        id: RunId,
    ) -> Result<RunHistory, CanonicalError> {
        self.inner.history(ctx, id).await
    }
    /// # Errors
    /// Propagates authorization, stale epoch and transition errors.
    pub async fn cancel(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        options: CancelOptions,
    ) -> Result<CancelResult, CanonicalError> {
        self.inner.cancel(ctx, id, options).await
    }
    /// # Errors
    /// Propagates authorization, stale epoch, contract and transition errors.
    pub async fn retry(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        options: ContinueOptions,
    ) -> Result<RunProgress, CanonicalError> {
        self.inner.retry(ctx, id, options).await
    }
    /// # Errors
    /// Propagates authorization, stale epoch, contract and transition errors.
    pub async fn resume(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        options: ContinueOptions,
    ) -> Result<RunProgress, CanonicalError> {
        self.inner.resume(ctx, id, options).await
    }
}
