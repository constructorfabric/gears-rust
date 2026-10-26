use crate::authorization::{Authority, AuthorizationErrorKind, CallerAction};
use crate::client::RunService;
use crate::error::codes as reason;
use crate::state::persisted::{Run, RunStatus};
use crate::{ContinueOptions, Error, ExecutionOwner, RunEvent, RunId, StartOptions, StartResult};
use crate::{
    error::domain::DomainError,
    state::journal::Journal,
    store::{JournalStore, StoreError},
};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use toolkit_security::{AccessScope, SecurityContext};
use uuid::Uuid;

/// Caller-scoped service behind `ExecutionClient` and `ExecutionInspector`.
pub struct Service {
    /// Authoritative journal store.
    pub store: JournalStore,
    /// Authorizes each caller operation on runs.
    pub authority: Authority,
}
impl Service {
    async fn continue_run(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        options: ContinueOptions,
        from: RunStatus,
    ) -> Result<Run, Error> {
        let scope = self.authorize(ctx, CallerAction::Update).await?;
        for _ in 0..8 {
            let mut journal = self.owned(ctx, &scope, id).await?;
            let registration = self.store.definition(&journal.run.definition).await?;
            if !registration.permits_execution(
                journal.registration_generation,
                journal.execution.is_compensation(),
            ) {
                return Err(DomainError::DefinitionInactive.into());
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
        Err(DomainError::ConcurrentUpdate.into())
    }
    async fn authorize(
        &self,
        ctx: &SecurityContext,
        action: CallerAction,
    ) -> Result<AccessScope, Error> {
        self.authorize_scope(ctx, action)
            .await
            .map(|scope| scope.ensure_owner(ctx.subject_id()))
    }
    /// PDP owns privilege decisions and the allowed tenant/owner/resource set.
    /// Subject types identify principals; they do not grant management access.
    async fn authorize_scope(
        &self,
        ctx: &SecurityContext,
        action: CallerAction,
    ) -> Result<AccessScope, Error> {
        self.authority.caller(ctx, action).await.map_err(|error| {
            let domain = match error.kind() {
                AuthorizationErrorKind::Denied => DomainError::Forbidden,
                AuthorizationErrorKind::Unavailable => DomainError::Unavailable,
            };
            Error::from(domain).with_source(error)
        })
    }
    /// Owners use the ordinary update permission. Cross-owner cancellation
    /// requires a separate PDP grant for manage and retains its row constraints.
    async fn journal_for_update(
        &self,
        ctx: &SecurityContext,
        id: RunId,
    ) -> Result<(AccessScope, Journal), Error> {
        match self.authorize(ctx, CallerAction::Update).await {
            Ok(scope) => match self.owned(ctx, &scope, id).await {
                Ok(journal) => return Ok((scope, journal)),
                Err(error) if matches!(error.domain(), DomainError::RunNotFound(_)) => {}
                Err(error) => return Err(error),
            },
            Err(error) if matches!(error.domain(), DomainError::Forbidden) => {}
            Err(error) => return Err(error),
        }
        let scope = self.authorize_scope(ctx, CallerAction::Manage).await?;
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
    ) -> Result<Journal, Error> {
        let journal = self
            .store
            .get(scope, id)
            .await?
            .ok_or(DomainError::RunNotFound(id))?;
        if journal.run.owner.subject_id != ctx.subject_id()
            || journal.run.owner.tenant_id != ctx.subject_tenant_id()
        {
            return Err(DomainError::RunNotFound(id).into());
        }
        Ok(journal)
    }
    async fn start_run(
        &self,
        ctx: &SecurityContext,
        name: &str,
        input: Value,
        options: StartOptions,
    ) -> Result<StartResult, Error> {
        crate::contracts::ExecutionContract::validate_name(name)?;
        let scope = self.authorize(ctx, CallerAction::Create).await?;
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
            }
            .into());
        }
        let definition = self.store.definition(name).await?.contract;
        if options
            .expected_fingerprint
            .as_ref()
            .is_some_and(|expected| definition.fingerprint().as_ref() != Ok(expected))
        {
            return Err(DomainError::DefinitionMismatch.into());
        }
        let owner = ExecutionOwner {
            tenant_id: ctx.subject_tenant_id(),
            subject_id: ctx.subject_id(),
        };
        let journal = Journal::new(RunId(Uuid::new_v4()), owner, &definition, input, Utc::now())?;
        Ok(self.store.start(scope, journal, options).await?)
    }
    async fn run_events(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        after: u64,
        limit: u32,
    ) -> Result<Vec<RunEvent>, Error> {
        let scope = self.authorize(ctx, CallerAction::Get).await?;
        self.owned(ctx, &scope, id).await?;
        Ok(self.store.events(&scope, id, after, limit).await?)
    }
    async fn cancel_run_at(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        expected_epoch: u64,
        reason: Option<String>,
    ) -> Result<bool, Error> {
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
        Err(DomainError::ConcurrentUpdate.into())
    }
}

/// Client boundary: every operation authorizes before reading or mutating journal state.
#[async_trait]
impl RunService for Service {
    async fn start(
        &self,
        ctx: &SecurityContext,
        name: &str,
        input: Value,
        options: StartOptions,
    ) -> Result<StartResult, Error> {
        self.start_run(ctx, name, input, options).await
    }
    async fn get(
        &self,
        ctx: &SecurityContext,
        id: RunId,
    ) -> Result<crate::observation::RunProgress, Error> {
        let scope = self.authorize(ctx, CallerAction::Get).await?;
        let progress = self
            .store
            .get_progress(&scope, id)
            .await?
            .ok_or(DomainError::RunNotFound(id))?;
        if progress.owner.subject_id != ctx.subject_id()
            || progress.owner.tenant_id != ctx.subject_tenant_id()
        {
            return Err(DomainError::RunNotFound(id).into());
        }
        Ok(progress)
    }
    async fn events(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        after: u64,
        limit: u32,
    ) -> Result<Vec<RunEvent>, Error> {
        self.run_events(ctx, id, after, limit).await
    }
    async fn history(
        &self,
        ctx: &SecurityContext,
        id: RunId,
    ) -> Result<crate::observation::RunHistory, Error> {
        let scope = self.authorize(ctx, CallerAction::Get).await?;
        let journal = self.owned(ctx, &scope, id).await?;
        Ok(crate::state::view::history(&journal))
    }
    async fn result(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        definition: &str,
        expected_fingerprint: &str,
        selector: crate::contracts::ResultSelector,
    ) -> Result<Value, Error> {
        self.read_result(ctx, id, definition, expected_fingerprint, selector)
            .await
    }
    async fn abort(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        options: crate::AbortOptions,
    ) -> Result<crate::CancelResult, Error> {
        for _ in 0..8 {
            let (scope, mut journal) = self.journal_for_update(ctx, id).await?;
            if !journal.request_abort_at(
                options.expected_epoch,
                Utc::now(),
                options.reason.as_deref(),
            )? {
                return Ok(crate::CancelResult::Unchanged);
            }
            let enqueue = journal.run.next_attempt_at.is_some();
            match self
                .store
                .save(scope, journal.revision, journal, enqueue)
                .await
            {
                Err(StoreError::Conflict) => {}
                result => {
                    result?;
                    return Ok(crate::CancelResult::Requested);
                }
            }
        }
        Err(DomainError::ConcurrentUpdate.into())
    }
    async fn cancel(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        options: crate::CancelOptions,
    ) -> Result<crate::CancelResult, Error> {
        self.cancel_run_at(ctx, id, options.expected_epoch, options.reason)
            .await
            .map(|changed| {
                if changed {
                    crate::CancelResult::Requested
                } else {
                    crate::CancelResult::Unchanged
                }
            })
    }
    async fn retry(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        options: ContinueOptions,
    ) -> Result<crate::observation::RunProgress, Error> {
        self.continue_run(ctx, id, options, RunStatus::Failed)
            .await?;
        RunService::get(self, ctx, id).await
    }
    async fn resume(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        options: ContinueOptions,
    ) -> Result<crate::observation::RunProgress, Error> {
        self.continue_run(ctx, id, options, RunStatus::Cancelled)
            .await?;
        RunService::get(self, ctx, id).await
    }
}
impl Service {
    /// Administrative progress of any run in the caller's inspect scope.
    pub(crate) async fn inspect(
        &self,
        ctx: &SecurityContext,
        id: RunId,
    ) -> Result<crate::observation::RunProgress, Error> {
        let scope = self.authorize_scope(ctx, CallerAction::Inspect).await?;
        self.store
            .get_progress(&scope, id)
            .await?
            .ok_or_else(|| DomainError::RunNotFound(id).into())
    }
    async fn read_result(
        &self,
        ctx: &SecurityContext,
        id: RunId,
        definition: &str,
        expected_fingerprint: &str,
        selector: crate::contracts::ResultSelector,
    ) -> Result<Value, Error> {
        let scope = self.authorize(ctx, CallerAction::Get).await?;
        let identity = self
            .store
            .result_identity(&scope, id)
            .await?
            .ok_or(DomainError::RunNotFound(id))?;
        if identity.tenant_id != ctx.subject_tenant_id() || identity.owner_id != ctx.subject_id() {
            return Err(DomainError::RunNotFound(id).into());
        }
        if identity.definition != definition || identity.fingerprint != expected_fingerprint {
            return Err(DomainError::DefinitionMismatch.into());
        }
        let expected_contract = self.store.definition(definition).await?.contract;
        if expected_contract.fingerprint()? != expected_fingerprint {
            return Err(DomainError::DefinitionMismatch.into());
        }
        let journal = self.owned(ctx, &scope, id).await?;
        if journal.run.definition != definition || journal.fingerprint != expected_fingerprint {
            return Err(DomainError::DefinitionMismatch.into());
        }
        let source = if let crate::contracts::ResultSelector::Checkpoint(source) = selector {
            let contract = &expected_contract;
            let ids = source.checkpoints();
            if ids.is_empty()
                || ids
                    .iter()
                    .any(|id| !contract.activities.iter().any(|a| a.id.0 == *id))
            {
                return Err(DomainError::DefinitionMismatch.into());
            }
            source
        } else {
            if journal.run.status != RunStatus::Succeeded {
                return Err(DomainError::InvalidState("workflow result is not ready").into());
            }
            let catalog = self.store.definition(definition).await?;
            if catalog.contract.fingerprint()? != journal.fingerprint {
                return Err(DomainError::DefinitionMismatch.into());
            }
            catalog.contract.flow.output
        };
        Ok(journal.execution.read(&source, &journal.input)?)
    }
    /// A scoped page of administrative progress projections.
    pub(crate) async fn list(
        &self,
        ctx: &SecurityContext,
        query: crate::observation::WorkflowQuery,
    ) -> Result<crate::observation::WorkflowPage, Error> {
        let scope = self.authorize_scope(ctx, CallerAction::List).await?;
        Ok(self.store.list_progress(&scope, &query).await?)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/service_tests.rs"]
mod tests;
