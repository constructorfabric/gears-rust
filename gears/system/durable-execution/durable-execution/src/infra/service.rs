use crate::domain::persisted::{Run, RunStatus};
use crate::{
    domain::{error::DomainError, journal::Journal},
    infra::storage::{JournalStore, StoreError},
};
use async_trait::async_trait;
use authz_resolver_sdk::{PolicyEnforcer, pep::ResourceType};
use chrono::Utc;
use durable_execution_sdk::contracts::InputSource;
use durable_execution_sdk::{
    ContinueOptions, DurableExecution, ExecutionOwner, RunEvent, RunId, StartOptions, StartResult,
    reason,
};
use serde_json::Value;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{AccessScope, SecurityContext, pep_properties};
use uuid::Uuid;

const RESOURCE: ResourceType = ResourceType::from_static(
    "durable_execution.run",
    &[
        pep_properties::OWNER_TENANT_ID,
        pep_properties::OWNER_ID,
        pep_properties::RESOURCE_ID,
    ],
);
pub struct Service {
    pub store: JournalStore,
    pub enforcer: PolicyEnforcer,
}
impl Service {
    async fn continue_run(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        options: ContinueOptions,
        from: RunStatus,
    ) -> Result<Run, DomainError> {
        let scope = self.authorize(ctx, "update").await?;
        for _ in 0..8 {
            let mut journal = self.owned(ctx, &scope, id).await?;
            let registration = self.store.definition(&journal.run.definition).await?;
            if !registration.permits(journal.registration_generation) {
                return Err(DomainError::DefinitionInactive);
            }
            let definition = registration.contract;
            if !journal.continue_execution(options.expected_epoch, from, &definition, Utc::now())? {
                return Ok(journal.run);
            }
            let result = journal.run.clone();
            match self
                .store
                .save(scope.clone(), journal.revision, journal, true)
                .await
            {
                Err(StoreError::Conflict) => {}
                saved => {
                    saved?;
                    return Ok(result);
                }
            }
        }
        Err(DomainError::ConcurrentUpdate)
    }
    async fn authorize(
        &self,
        ctx: &SecurityContext,
        action: &str,
    ) -> Result<AccessScope, DomainError> {
        self.authorize_scope(ctx, action)
            .await
            .map(|scope| scope.ensure_owner(ctx.subject_id()))
    }
    /// PDP owns privilege decisions and the allowed tenant/owner/resource set.
    /// Subject types identify principals; they do not grant management access.
    async fn authorize_scope(
        &self,
        ctx: &SecurityContext,
        action: &str,
    ) -> Result<AccessScope, DomainError> {
        if ctx.subject_id().is_nil() || ctx.subject_tenant_id().is_nil() {
            return Err(DomainError::Forbidden);
        }
        self.enforcer
            .access_scope(ctx, &RESOURCE, action, None)
            .await
            .map_err(|error| {
                super::authorization::warn_policy_failure(&error, action, RESOURCE.name(), ctx);
                match error {
                    authz_resolver_sdk::pep::EnforcerError::Denied { .. } => DomainError::Forbidden,
                    _ => DomainError::Unavailable,
                }
            })
    }
    /// Owners use the ordinary update permission. Cross-owner cancellation
    /// requires a separate PDP grant for manage and retains its row constraints.
    async fn journal_for_update(
        &self,
        ctx: &SecurityContext,
        id: RunId,
    ) -> Result<(AccessScope, Journal), DomainError> {
        match self.authorize(ctx, "update").await {
            Ok(scope) => match self.owned(ctx, &scope, id).await {
                Ok(journal) => return Ok((scope, journal)),
                Err(DomainError::RunNotFound(_)) => {}
                Err(error) => return Err(error),
            },
            Err(DomainError::Forbidden) => {}
            Err(error) => return Err(error),
        }
        let scope = self.authorize_scope(ctx, "manage").await?;
        let journal = self
            .store
            .get(&scope, id)
            .await?
            .ok_or(DomainError::RunNotFound(id))?;
        Ok((scope, journal))
    }
    async fn owned(
        &self,
        ctx: &SecurityContext,
        scope: &AccessScope,
        id: RunId,
    ) -> Result<Journal, DomainError> {
        let journal = self
            .store
            .get(scope, id)
            .await?
            .ok_or(DomainError::RunNotFound(id))?;
        if journal.run.owner.subject_id != ctx.subject_id()
            || journal.run.owner.tenant_id != ctx.subject_tenant_id()
        {
            return Err(DomainError::RunNotFound(id));
        }
        Ok(journal)
    }
    async fn start_run(
        &self,
        ctx: &SecurityContext,
        name: &str,
        input: Value,
        options: StartOptions,
    ) -> Result<StartResult, DomainError> {
        durable_execution_sdk::contracts::ExecutionContract::validate_name(name)?;
        let scope = self.authorize(ctx, "create").await?;
        let encoded = serde_json::to_vec(&input).map_err(|_| DomainError::InvalidRequest {
            field: "input",
            reason: reason::INVALID_FORMAT,
            message: "input cannot be encoded",
        })?;
        if encoded.len() > 262_144 {
            return Err(DomainError::InvalidRequest {
                field: "input",
                reason: reason::PAYLOAD_TOO_LARGE,
                message: "input exceeds 256 KiB",
            });
        }
        let definition = self.store.definition(name).await?.contract;
        if options
            .expected_fingerprint
            .as_ref()
            .is_some_and(|expected| definition.fingerprint().as_ref() != Ok(expected))
        {
            return Err(DomainError::DefinitionMismatch);
        }
        let owner = ExecutionOwner {
            tenant_id: ctx.subject_tenant_id(),
            subject_id: ctx.subject_id(),
        };
        let journal = Journal::new(RunId(Uuid::new_v4()), owner, &definition, input, Utc::now())?;
        self.store.start(scope, journal, options).await
    }
    async fn run_events(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        after: u64,
        limit: u32,
    ) -> Result<Vec<RunEvent>, DomainError> {
        let scope = self.authorize(ctx, "get").await?;
        self.owned(ctx, &scope, id).await?;
        Ok(self.store.events(&scope, id, after, limit).await?)
    }
    async fn cancel_run_at(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        expected_epoch: u64,
        reason: Option<String>,
    ) -> Result<bool, DomainError> {
        for _ in 0..8 {
            let (scope, mut journal) = self.journal_for_update(ctx, id).await?;
            if !journal.request_cancel_at(expected_epoch, Utc::now(), reason.as_deref())? {
                return Ok(false);
            }
            match self
                .store
                .save(scope, journal.revision, journal, false)
                .await
            {
                Err(StoreError::Conflict) => {}
                result => return Ok(result.map(|()| true)?),
            }
        }
        Err(DomainError::ConcurrentUpdate)
    }
}

/// SDK boundary: every operation authorizes before reading or mutating journal state.
#[async_trait]
impl DurableExecution for Service {
    async fn start(
        &self,
        ctx: &SecurityContext,
        name: &str,
        input: Value,
        options: StartOptions,
    ) -> Result<StartResult, CanonicalError> {
        self.start_run(ctx, name, input, options)
            .await
            .map_err(Into::into)
    }
    async fn get(
        &self,
        ctx: &SecurityContext,
        id: RunId,
    ) -> Result<durable_execution_sdk::observation::RunProgress, CanonicalError> {
        let scope = self
            .authorize(ctx, "get")
            .await
            .map_err(CanonicalError::from)?;
        let journal = self
            .owned(ctx, &scope, id)
            .await
            .map_err(CanonicalError::from)?;
        crate::domain::view::progress(&journal).map_err(CanonicalError::from)
    }
    async fn events(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        after: u64,
        limit: u32,
    ) -> Result<Vec<RunEvent>, CanonicalError> {
        self.run_events(ctx, id, after, limit)
            .await
            .map_err(Into::into)
    }
    async fn history(
        &self,
        ctx: &SecurityContext,
        id: RunId,
    ) -> Result<durable_execution_sdk::observation::RunHistory, CanonicalError> {
        let scope = self
            .authorize(ctx, "get")
            .await
            .map_err(CanonicalError::from)?;
        let journal = self
            .owned(ctx, &scope, id)
            .await
            .map_err(CanonicalError::from)?;
        Ok(crate::domain::view::history(&journal))
    }
    async fn result(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        definition: &str,
        source: Option<durable_execution_sdk::contracts::InputSource>,
    ) -> Result<Value, CanonicalError> {
        self.read_result(ctx, id, definition, source)
            .await
            .map_err(Into::into)
    }
    async fn cancel(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        options: durable_execution_sdk::CancelOptions,
    ) -> Result<durable_execution_sdk::CancelResult, CanonicalError> {
        self.cancel_run_at(ctx, id, options.expected_epoch, options.reason)
            .await
            .map(|changed| {
                if changed {
                    durable_execution_sdk::CancelResult::Requested
                } else {
                    durable_execution_sdk::CancelResult::Unchanged
                }
            })
            .map_err(Into::into)
    }
    async fn retry(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        options: ContinueOptions,
    ) -> Result<durable_execution_sdk::observation::RunProgress, CanonicalError> {
        self.continue_run(ctx, id, options, RunStatus::Failed)
            .await
            .map_err(CanonicalError::from)?;
        DurableExecution::get(self, ctx, id).await
    }
    async fn resume(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        options: ContinueOptions,
    ) -> Result<durable_execution_sdk::observation::RunProgress, CanonicalError> {
        self.continue_run(ctx, id, options, RunStatus::Cancelled)
            .await
            .map_err(CanonicalError::from)?;
        DurableExecution::get(self, ctx, id).await
    }
}
#[async_trait]
impl durable_execution_sdk::ExecutionInspector for Service {
    async fn inspect(
        &self,
        ctx: &SecurityContext,
        id: RunId,
    ) -> Result<durable_execution_sdk::observation::RunProgress, CanonicalError> {
        let scope = self
            .authorize_scope(ctx, "inspect")
            .await
            .map_err(CanonicalError::from)?;
        let journal = self
            .store
            .get(&scope, id)
            .await
            .map_err(DomainError::from)?
            .ok_or(DomainError::RunNotFound(id))?;
        crate::domain::view::progress(&journal).map_err(CanonicalError::from)
    }
    async fn list(
        &self,
        ctx: &SecurityContext,
        query: durable_execution_sdk::observation::WorkflowQuery,
    ) -> Result<durable_execution_sdk::observation::WorkflowPage, CanonicalError> {
        self.list_progress(ctx, query).await.map_err(Into::into)
    }
}
impl Service {
    async fn read_result(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        definition: &str,
        source: Option<durable_execution_sdk::contracts::InputSource>,
    ) -> Result<Value, DomainError> {
        use durable_execution_sdk::contracts::InputSource;
        let scope = self.authorize(ctx, "get").await?;
        let journal = self.owned(ctx, &scope, id).await?;
        if journal.run.definition != definition {
            return Err(DomainError::DefinitionMismatch);
        }
        let source = if let Some(source) = source {
            source
        } else {
            if journal.run.status != RunStatus::Succeeded {
                return Err(DomainError::InvalidState("workflow result is not ready"));
            }
            let catalog = self.store.definition(definition).await?;
            if catalog.contract.fingerprint()? != journal.fingerprint {
                return Err(DomainError::DefinitionMismatch);
            }
            catalog.contract.flow.map_or_else(
                || {
                    InputSource::Checkpoint(
                        journal
                            .run
                            .activities
                            .last()
                            .map(|a| a.id.0.clone())
                            .unwrap_or_default(),
                    )
                },
                |flow| flow.output,
            )
        };
        read_checkpoint(&source, &journal)
    }
    async fn list_progress(
        &self,
        ctx: &SecurityContext,
        query: durable_execution_sdk::observation::WorkflowQuery,
    ) -> Result<durable_execution_sdk::observation::WorkflowPage, DomainError> {
        let scope = self.authorize_scope(ctx, "list").await?;
        Ok(self.store.list_progress(&scope, &query).await?)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/service_tests.rs"]
mod tests;

fn read_checkpoint(source: &InputSource, journal: &Journal) -> Result<Value, DomainError> {
    match source {
        InputSource::RunInput => Err(DomainError::InvalidState(
            "run inputs are not execution results",
        )),
        InputSource::Checkpoint(id) => journal
            .run
            .activities
            .iter()
            .find(|a| {
                a.id.0 == *id && a.status == crate::domain::persisted::ActivityStatus::Succeeded
            })
            .and_then(|a| a.result.clone())
            .ok_or(DomainError::InvalidState("checkpoint is not ready")),
        InputSource::Tuple(items) | InputSource::List(items) => items
            .iter()
            .map(|s| read_checkpoint(s, journal))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
    }
}
