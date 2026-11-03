//! The catalog row is the admission/revocation serialization point.
//! Locks are scoped SQL queries; no database transaction spans handler work.
use super::{JournalStore, StoreError, definition, normalized, run};
use crate::authorization::ServiceAction;
use crate::catalog::definition::Definition;
use crate::contracts::ExecutionContract;
use crate::error::domain::DomainError;
use crate::registration::{RegistrationState, UnregisterOptions};
use crate::state::journal::Journal;
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait, sea_query::Expr};
use toolkit_db::secure::{DBRunner, SecureEntityExt, SecureUpdateExt, secure_insert};
use toolkit_security::AccessScope;
use uuid::Uuid;

pub(super) async fn locked<C: DBRunner>(
    txn: &C,
    scope: &AccessScope,
    name: &str,
    exclusive: bool,
    postgres: bool,
) -> Result<Option<Definition>, StoreError> {
    let query = definition::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(definition::Column::Name.eq(name)));
    let query = if postgres {
        if exclusive {
            query.lock_exclusive()
        } else {
            query.lock_shared()
        }
    } else {
        query
    };
    query.one(txn).await?.as_ref().map(decode).transpose()
}
fn decode(row: &definition::Model) -> Result<Definition, StoreError> {
    let value: Definition = serde_json::from_slice(&row.state)?;
    if value.contract.name != row.name || i64::try_from(value.revision).ok() != Some(row.revision) {
        return Err(StoreError::Invariant(
            "definition row does not match its state",
        ));
    }
    Ok(value)
}
async fn persist<C: DBRunner>(
    txn: &C,
    scope: &AccessScope,
    before: u64,
    value: &Definition,
) -> Result<(), StoreError> {
    let changed = definition::Entity::update_many()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(definition::Column::Name.eq(&value.contract.name))
                .add(definition::Column::Revision.eq(number(before)?)),
        )
        .col_expr(
            definition::Column::Revision,
            Expr::value(number(value.revision)?),
        )
        .col_expr(
            definition::Column::State,
            Expr::value(serde_json::to_vec(value)?),
        )
        .exec(txn)
        .await?;
    if changed.rows_affected != 1 {
        return Err(StoreError::Conflict);
    }
    Ok(())
}
fn number(n: u64) -> Result<i64, StoreError> {
    i64::try_from(n).map_err(|_| StoreError::Invariant("value out of range"))
}
impl JournalStore {
    pub(super) fn postgres(&self) -> bool {
        self.db.db().backend() == sea_orm::DbBackend::Postgres
    }
    /// Registers `contract`, or returns the identical existing definition; a mismatch conflicts.
    pub(crate) async fn ensure_contract(
        &self,
        contract: ExecutionContract,
    ) -> Result<Definition, crate::Error> {
        contract.validate()?;
        let scope = self
            .worker_scope(ServiceAction::DefinitionRegister)
            .await
            .map_err(crate::Error::for_definition_store)?;
        self.ensure_contract_with_scope(contract, scope)
            .await
            .map_err(crate::Error::for_definition_store)
    }
    pub(super) async fn ensure_contract_with_scope(
        &self,
        contract: ExecutionContract,
        scope: AccessScope,
    ) -> Result<Definition, StoreError> {
        let postgres = self.postgres();
        for _ in 0..8 {
            let contract = contract.clone();
            let scope = scope.clone();
            let result = self
                .db
                .db()
                .transaction_ref_mapped(move |txn| {
                    Box::pin(async move {
                        if let Some(value) =
                            locked(txn, &scope, &contract.name, true, postgres).await?
                        {
                            if value.contract != contract {
                                return Err(StoreError::Domain(DomainError::DefinitionConflict(
                                    contract.name.clone(),
                                )));
                            }
                            return Ok(value);
                        }
                        let value = Definition::new(contract);
                        secure_insert::<definition::Entity>(
                            definition::ActiveModel {
                                id: Set(Uuid::new_v5(
                                    &Uuid::NAMESPACE_OID,
                                    value.contract.name.as_bytes(),
                                )),
                                name: Set(value.contract.name.clone()),
                                revision: Set(0),
                                state: Set(serde_json::to_vec(&value)?),
                            },
                            &scope,
                            txn,
                        )
                        .await?;
                        Ok(value)
                    })
                })
                .await;
            match result {
                Err(ref e) if e.retryable_race() => tokio::task::yield_now().await,
                result => return result,
            }
        }
        Err(StoreError::Conflict)
    }
    /// Reads a definition by name, mapping failures to definition-level domain errors.
    pub(crate) async fn definition(&self, name: &str) -> Result<Definition, crate::Error> {
        self.worker_definition(name)
            .await
            .map_err(crate::Error::for_definition_store)
    }
    /// Reads a validated definition by name under the worker scope, as a store error.
    pub(crate) async fn worker_definition(&self, name: &str) -> Result<Definition, StoreError> {
        ExecutionContract::validate_name(name).map_err(DomainError::from)?;
        let scope = self.worker_scope(ServiceAction::DefinitionGet).await?;
        locked(&self.db.conn()?, &scope, name, false, false)
            .await?
            .ok_or_else(|| StoreError::Domain(DomainError::DefinitionNotFound(name.to_owned())))
    }
    #[cfg(test)]
    #[cfg_attr(coverage_nightly, coverage(off))]
    pub(crate) async fn definition_on<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
        name: &str,
    ) -> Result<Definition, crate::Error> {
        ExecutionContract::validate_name(name)?;
        locked(runner, scope, name, false, false)
            .await?
            .ok_or_else(|| DomainError::DefinitionNotFound(name.to_owned()).into())
    }
    /// Revokes a definition under the exclusive catalog lock at `options.expected_revision`.
    pub(crate) async fn unregister(
        &self,
        name: &str,
        options: UnregisterOptions,
    ) -> Result<Definition, crate::Error> {
        let scope = self
            .worker_scope(ServiceAction::DefinitionUnregister)
            .await
            .map_err(crate::Error::for_definition_store)?;
        if options.mode == crate::registration::UnregisterMode::CancelAndRelease {
            self.cancel_definition_scope().await?;
        }
        let postgres = self.postgres();
        let name = name.to_owned();
        self.db
            .db()
            .transaction_ref_mapped(move |txn| {
                Box::pin(async move {
                    let mut value = locked(txn, &scope, &name, true, postgres)
                        .await?
                        .ok_or_else(|| {
                            StoreError::Domain(DomainError::DefinitionNotFound(name.clone()))
                        })?;
                    value.unregister(options.expected_revision, options.mode)?;
                    persist(txn, &scope, options.expected_revision, &value).await?;
                    Ok::<_, StoreError>(value)
                })
            })
            .await
            .map_err(crate::Error::for_definition_store)
    }
    /// Reactivates a definition under the exclusive catalog lock; `revision` is the CAS token.
    pub(crate) async fn activate(
        &self,
        name: &str,
        revision: u64,
    ) -> Result<Definition, crate::Error> {
        let scope = self
            .worker_scope(ServiceAction::DefinitionActivate)
            .await
            .map_err(crate::Error::for_definition_store)?;
        let postgres = self.postgres();
        let name = name.to_owned();
        self.db
            .db()
            .transaction_ref_mapped(move |txn| {
                Box::pin(async move {
                    let mut value = locked(txn, &scope, &name, true, postgres)
                        .await?
                        .ok_or_else(|| {
                            StoreError::Domain(DomainError::DefinitionNotFound(name.clone()))
                        })?;
                    value.activate(revision)?;
                    persist(txn, &scope, revision, &value).await?;
                    Ok::<_, StoreError>(value)
                })
            })
            .await
            .map_err(crate::Error::for_definition_store)
    }
    async fn cancel_definition_scope(&self) -> Result<AccessScope, StoreError> {
        let scope = self.worker_scope(ServiceAction::CancelDefinition).await?;
        if !scope.is_unconstrained() {
            return Err(StoreError::Forbidden);
        }
        Ok(scope)
    }
    /// Lists all catalog definitions under the worker listing scope.
    pub(crate) async fn definitions(&self) -> Result<Vec<Definition>, StoreError> {
        let scope = self.worker_scope(ServiceAction::DefinitionList).await?;
        self.definitions_on(&self.db.conn()?, &scope).await
    }
    /// Lists all catalog definitions visible to `scope` on `runner`.
    pub(crate) async fn definitions_on<C: DBRunner>(
        &self,
        runner: &C,
        scope: &AccessScope,
    ) -> Result<Vec<Definition>, StoreError> {
        definition::Entity::find()
            .secure()
            .scope_with(scope)
            .all(runner)
            .await?
            .iter()
            .map(decode)
            .collect()
    }
    /// Cancels runs of a `Stopping` definition and releases it once none remain claimed.
    pub(crate) async fn reconcile_definition(&self, value: &Definition) -> Result<(), StoreError> {
        if value.state != RegistrationState::Stopping {
            return Ok(());
        }
        let scope = self.cancel_definition_scope().await?;
        let rows = run::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(
                Condition::all()
                    .add(run::Column::Definition.eq(&value.contract.name))
                    .add(run::Column::RegistrationGeneration.lte(number(value.generation)?))
                    .add(run::Column::Status.is_not_in(if value.contract.saga {
                        vec!["succeeded", "compensated"]
                    } else {
                        vec!["succeeded", "failed", "cancelled", "compensated"]
                    })),
            )
            .limit(100)
            .all(&self.db.conn()?)
            .await?;
        for row in rows {
            // Refresh global permission between mutations; an outage must retain Stopping.
            let scope = self.cancel_definition_scope().await?;
            let mut journal = match normalized::hydrate(&self.db.conn()?, &scope, row).await {
                Ok(journal) => journal,
                Err(StoreError::Conflict) => continue,
                Err(error) => return Err(error),
            };
            let revision = journal.revision;
            let cancelled = if journal.execution.contract.saga {
                journal.request_abort_at(
                    journal.run.execution_epoch,
                    chrono::Utc::now(),
                    Some("definition_unregistered"),
                )?
            } else {
                journal.request_cancel_at(
                    journal.run.execution_epoch,
                    chrono::Utc::now(),
                    Some("definition_unregistered"),
                )?
            };
            let recovered = journal.recover(chrono::Utc::now())?;
            if !cancelled && !recovered {
                continue;
            }
            let enqueue = journal.run.next_attempt_at.is_some();
            match self.save(scope, revision, journal, enqueue).await {
                Err(StoreError::Conflict) => {}
                result => result?,
            }
        }
        let scope = self.cancel_definition_scope().await?;
        let catalog_scope = self.worker_scope(ServiceAction::DefinitionRelease).await?;
        let postgres = self.postgres();
        let name = value.contract.name.clone();
        let expected = value.revision;
        self.db
            .db()
            .transaction_ref_mapped(move |txn| {
                Box::pin(async move {
                    let mut current = locked(txn, &catalog_scope, &name, true, postgres)
                        .await?
                        .ok_or_else(|| {
                            StoreError::Domain(DomainError::DefinitionNotFound(name.clone()))
                        })?;
                    if current.revision != expected || current.state != RegistrationState::Stopping
                    {
                        return Ok(());
                    }
                    // Unconstrained scan under exclusive catalog lock: no new claim/admission can pass.
                    let remaining = run::Entity::find()
                        .secure()
                        .scope_with(&scope)
                        .filter(
                            Condition::all()
                                .add(run::Column::Definition.eq(&name))
                                .add(
                                    run::Column::RegistrationGeneration
                                        .lte(number(current.generation)?),
                                )
                                .add(
                                    Condition::any()
                                        .add(run::Column::Status.is_not_in(
                                            if current.contract.saga {
                                                vec!["succeeded", "compensated"]
                                            } else {
                                                vec![
                                                    "succeeded",
                                                    "failed",
                                                    "cancelled",
                                                    "compensated",
                                                ]
                                            },
                                        ))
                                        .add(run::Column::LeaseUntil.is_not_null()),
                                ),
                        )
                        .count(txn)
                        .await?;
                    if remaining != 0 {
                        return Ok(());
                    }
                    current.release()?;
                    persist(txn, &catalog_scope, expected, &current).await
                })
            })
            .await
    }
}
/// Revoked generations may only cancel/release claims, never advance effects or epochs.
pub(super) fn cancellation_only(previous: &Journal, next: &Journal) -> bool {
    let (old, new) = (&previous.execution, &next.execution);
    if new.cause().is_some() {
        // Revocation permits reverse cleanup only. No newly confirmed forward effect can pass.
        return old
            .instances
            .iter()
            .filter(|i| i.identity.phase == crate::ExecutionPhase::Forward)
            .all(|i| {
                new.instances
                    .iter()
                    .find(|n| n.identity == i.identity)
                    .is_some_and(|n| {
                        n.state.result == i.state.result && n.state.attempts == i.state.attempts
                    })
            })
            && old.contract == new.contract
            && new
                .instances
                .iter()
                .filter(|i| i.identity.phase == crate::ExecutionPhase::Forward)
                .count()
                == old
                    .instances
                    .iter()
                    .filter(|i| i.identity.phase == crate::ExecutionPhase::Forward)
                    .count()
            && previous.run.owner == next.run.owner
            && previous.run.definition == next.run.definition
            && old.cause().is_none_or(|cause| new.cause() == Some(cause));
    }

    next.cancellation_requested
        && next.run.execution_epoch == previous.run.execution_epoch
        && matches!(
            next.run.status,
            crate::state::persisted::RunStatus::Cancelling
                | crate::state::persisted::RunStatus::Cancelled
        )
        && next.run.activities.len() == previous.run.activities.len()
        && next
            .run
            .activities
            .iter()
            .zip(&previous.run.activities)
            .all(|(a, b)| {
                a.attempts == b.attempts
                    && a.result == b.result
                    && (a.status != crate::state::persisted::ActivityStatus::Succeeded
                        || a.status == b.status)
            })
        && next.lease_until <= previous.lease_until
        && new.instances.len() == old.instances.len()
        && new
            .instances
            .iter()
            .zip(&old.instances)
            .all(|(a, b)| a.lease_until <= b.lease_until)
}

pub(super) async fn admit<C: DBRunner>(
    txn: &C,
    scope: &AccessScope,
    journal: &mut Journal,
    postgres: bool,
) -> Result<(), StoreError> {
    let registration = locked(txn, scope, &journal.run.definition, false, postgres)
        .await?
        .ok_or(StoreError::DefinitionInactive)?;
    if registration.state != RegistrationState::Active {
        return Err(StoreError::DefinitionInactive);
    }
    if registration
        .contract
        .fingerprint()
        .map_err(|_| StoreError::Invariant("stored contract is invalid"))?
        != journal.fingerprint
    {
        return Err(StoreError::Domain(DomainError::DefinitionMismatch));
    }
    journal.registration_generation = registration.generation;
    Ok(())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/catalog_reconciliation_tests.rs"]
mod tests;
