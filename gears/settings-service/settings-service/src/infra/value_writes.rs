// Created: 2026-09-07 by Constructor Tech
// @cpt-dod:cpt-cf-settings-service-dod-value-writes-batch:p1
//! The write coordinator: one transaction per change, commit, evict, publish.
//!
//! The domain writer decides and commits inside a transaction it is handed;
//! this adapter owns the transaction and what follows it, and shapes the
//! request-level operations — a batch, a clone, the read-only report — out of
//! the writer's steps.

use std::sync::Arc;

use serde_json::Value;
use settings_service_sdk::SettingKey;
use toolkit_db::{DBProvider, DbError};
use uuid::Uuid;

use crate::domain::declaration::Declaration;
use crate::domain::error::DomainError;
use crate::domain::resolution::{EffectiveValue, ScopeTarget};
use crate::domain::writes::service::{ImpactReport, StepUpPolicy, ValidationReport};
use crate::domain::writes::{Change, Committed, Gated, ValueWriter, WriteActor};
use crate::field;
use crate::infra::storage::access_repo::AccessRepo;
use crate::infra::storage::audit_store::AuditStore;
use crate::infra::storage::declaration_repo::DeclarationRepo;
use crate::infra::storage::value_repo::ValueRepo;

/// The writer over the concrete repositories and the audit store.
pub type ConcreteWriter = ValueWriter<DeclarationRepo, ValueRepo, AccessRepo, AuditStore>;

/// The most changes one batch may carry.
pub const BATCH_LIMIT: usize = 500;

/// One change of a batch.
#[derive(Debug, Clone)]
pub struct BatchChange {
    /// The setting.
    pub key: SettingKey,
    /// The target tenant; absent, the caller's own.
    pub tenant: Option<Uuid>,
    /// The value.
    pub value: Value,
    /// The tag the caller last read.
    pub if_match: Option<String>,
}

/// One batch's answer: a change set id and one outcome per change.
#[derive(Debug)]
pub struct BatchOutcome {
    /// The change set every committed change carries.
    pub change_set_id: Uuid,
    /// In request order.
    pub results: Vec<Result<Committed, DomainError>>,
}

/// The coordinator.
pub struct WriteCoordinator {
    db: Arc<DBProvider<DbError>>,
    writer: Arc<ConcreteWriter>,
}

fn conn_error(err: &DbError) -> DomainError {
    DomainError::Internal {
        diagnostic: err.to_string(),
    }
}

impl WriteCoordinator {
    /// Over this database and writer.
    #[must_use]
    pub fn new(db: Arc<DBProvider<DbError>>, writer: Arc<ConcreteWriter>) -> Self {
        Self { db, writer }
    }

    /// The writer, for the challenge parameters and the resolver.
    #[must_use]
    pub fn writer(&self) -> &Arc<ConcreteWriter> {
        &self.writer
    }

    /// Gate, commit in one transaction, then evict and publish.
    ///
    /// # Errors
    /// The first gate's refusal, or the commit's rejection; a rejection is also
    /// published as a durable failure notification.
    pub async fn change(
        &self,
        actor: &WriteActor,
        key: &SettingKey,
        requested: Option<Uuid>,
        change: Change,
        if_match: Option<&str>,
        operation: &'static str,
    ) -> Result<Committed, DomainError> {
        let conn = self.db.conn().map_err(|e| conn_error(&e))?;
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-set:p1:inst-vw-set-2
        let gated = self
            .writer
            .gate(
                &conn,
                actor,
                key,
                requested,
                StepUpPolicy::Verify,
                operation,
            )
            .await?;
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-set:p1:inst-vw-set-2
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-set:p1:inst-vw-set-5
        // One change set per request, carried on the record.
        let change_set_id = Uuid::new_v4();
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-set:p1:inst-vw-set-5
        self.commit(actor, gated, change, if_match, change_set_id)
            .await
    }

    /// Commit a gated change in its own transaction and run what follows.
    ///
    /// # Errors
    /// The commit's rejection, after it was published.
    pub async fn commit(
        &self,
        actor: &WriteActor,
        gated: Gated,
        change: Change,
        if_match: Option<&str>,
        change_set_id: Uuid,
    ) -> Result<Committed, DomainError> {
        let key = gated.declaration.key.clone();
        let tenant_id = gated.tenant_id;
        // Validation and the store leg come first, outside the transaction: the
        // Credential Store cannot join it, and toolkit-db refuses a fresh
        // connection while one is open on this task.
        let staged = match self.writer.stage(&gated, change).await {
            Ok(staged) => staged,
            Err(err) => {
                self.writer
                    .after_rejection(&key, tenant_id, actor, &err)
                    .await;
                return Err(err);
            }
        };
        let writer = Arc::clone(&self.writer);
        let actor_owned = actor.clone();
        let if_match = if_match.map(str::to_owned);
        let gated_owned = gated.clone();
        let staged_owned = staged.clone();
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-set:p1:inst-vw-set-3
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-set:p1:inst-vw-set-4
        let outcome = self
            .db
            .db()
            .transaction_ref_mapped::<_, Committed, DomainError>(move |tx| {
                Box::pin(async move {
                    writer
                        .commit_in(
                            tx,
                            &gated_owned,
                            &staged_owned,
                            if_match.as_deref(),
                            &actor_owned,
                            change_set_id,
                        )
                        .await
                })
            })
            .await;
        match outcome {
            Ok(committed) => {
                self.writer.after_commit(&committed, actor).await;
                Ok(committed)
            }
            Err(err) => {
                self.writer.discard(&gated, &staged).await;
                self.writer
                    .after_rejection(&key, tenant_id, actor, &err)
                    .await;
                Err(err)
            }
        }
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-set:p1:inst-vw-set-4
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-set:p1:inst-vw-set-3
    }

    /// Several changes, each standing or falling alone; step-up verified once.
    ///
    /// # Errors
    /// [`DomainError::Validation`] over the size limit;
    /// [`DomainError::StepUpRequired`] or [`DomainError::Unauthorized`] when the
    /// request-level step-up fails, nothing evaluated; a database failure.
    pub async fn batch(
        &self,
        actor: &WriteActor,
        changes: Vec<BatchChange>,
    ) -> Result<BatchOutcome, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-2
        if changes.len() > BATCH_LIMIT {
            return Err(DomainError::Validation {
                field: "changes".to_owned(),
                code: field::VALIDATION,
                message: format!("a batch carries at most {BATCH_LIMIT} changes"),
            });
        }
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-2
        let conn = self.db.conn().map_err(|e| conn_error(&e))?;
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-3
        // Once for the request: the declarations that can be read decide
        // whether a person must have re-authenticated; a key that cannot is
        // reported in its own entry below.
        let root = self.writer.resolver().root_tenant().await?;
        let mut declarations: Vec<Declaration> = Vec::new();
        for change in &changes {
            if let Ok(declaration) = self
                .writer
                .declaration_for_write(&conn, actor, &change.key, root)
                .await
            {
                declarations.push(declaration);
            }
        }
        if ConcreteWriter::any_requires_step_up(&declarations) {
            if !actor.is_interactive() {
                return Err(DomainError::Unauthorized {
                    resource: settings_service_sdk::gts::VALUE_SCHEMA,
                });
            }
            self.writer.verify_step_up(actor, "batch").await?;
        }
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-3
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-4
        let change_set_id = Uuid::new_v4();
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-4
        let mut results = Vec::with_capacity(changes.len());
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-5
        for change in changes {
            // @cpt-begin:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-6
            let gated = self
                .writer
                .gate(
                    &conn,
                    actor,
                    &change.key,
                    change.tenant,
                    StepUpPolicy::AlreadyVerified,
                    "batch",
                )
                .await;
            let outcome = match gated {
                Ok(gated) => {
                    self.commit(
                        actor,
                        gated,
                        Change::Set(change.value),
                        change.if_match.as_deref(),
                        change_set_id,
                    )
                    .await
                }
                Err(err) => Err(err),
            };
            // @cpt-end:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-6
            // @cpt-begin:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-7
            results.push(outcome);
            // @cpt-end:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-7
        }
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-5
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-8
        // Eviction and publication ran per committed change, after each
        // commit and before the next; nothing here is observable before it is
        // stored.
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-8
        Ok(BatchOutcome {
            change_set_id,
            results,
        })
    }

    /// Copy the effective value at `from` as an override at `to`.
    ///
    /// The caller authorized `read` for the source and `write` for the target
    /// before this point.
    ///
    /// # Errors
    /// [`DomainError::Unauthorized`] when the source is outside the caller's
    /// subtree; the target gates' refusals; [`DomainError::Validation`] with
    /// `secret_not_cloneable` for a secret setting; the commit's rejection.
    pub async fn clone_value(
        &self,
        actor: &WriteActor,
        key: &SettingKey,
        from: Option<Uuid>,
        to: Option<Uuid>,
        if_match: Option<&str>,
    ) -> Result<Committed, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-clone:p1:inst-vw-clone-2
        let (source, _, _) = self.writer.target_for(actor, from).await?;
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-clone:p1:inst-vw-clone-2
        let conn = self.db.conn().map_err(|e| conn_error(&e))?;
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-clone:p1:inst-vw-clone-3
        let gated = self
            .writer
            .gate(&conn, actor, key, to, StepUpPolicy::Verify, "clone")
            .await?;
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-clone:p1:inst-vw-clone-3
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-clone:p1:inst-vw-clone-4
        // Copying a secret reference would couple the target to the source
        // credential's lifecycle; a secret is set at the target instead.
        if gated.declaration.data_classification == "secret" {
            return Err(DomainError::Validation {
                field: "key".to_owned(),
                code: field::SECRET_NOT_CLONEABLE,
                message: format!("`{key}` is secret-classified and cannot be cloned"),
            });
        }
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-clone:p1:inst-vw-clone-4
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-clone:p1:inst-vw-clone-5
        let effective = self.writer.resolver().resolve(&conn, key, source).await?;
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-clone:p1:inst-vw-clone-5
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-clone:p1:inst-vw-clone-6
        // The effective value, copied once: no continuing link to the source.
        self.commit(
            actor,
            gated,
            Change::Set(effective.value.clone()),
            if_match,
            Uuid::new_v4(),
        )
        .await
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-clone:p1:inst-vw-clone-6
    }

    /// The read-only report for a candidate value.
    ///
    /// # Errors
    /// [`DomainError::Unauthorized`] for a target outside the subtree;
    /// not-found or retired for the declaration; a resolver failure.
    pub async fn validate(
        &self,
        actor: &WriteActor,
        key: &SettingKey,
        requested: Option<Uuid>,
        value: &Value,
        limit: Option<usize>,
    ) -> Result<ValidationReport, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-validate:p1:inst-vw-val-3
        let (target, _, root) = self.writer.target_for(actor, requested).await?;
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-validate:p1:inst-vw-val-3
        let conn = self.db.conn().map_err(|e| conn_error(&e))?;
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-validate:p1:inst-vw-val-4
        let declaration = self
            .writer
            .declaration_for_write(&conn, actor, key, root)
            .await?;
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-validate:p1:inst-vw-val-4
        self.writer
            .validate(&conn, &declaration, target, value, limit)
            .await
    }

    /// The bounded impact report for a candidate value, with the declaration's
    /// classification so the caller masks each descendant's current value as a
    /// read would.
    ///
    /// # Errors
    /// As [`Self::validate`].
    pub async fn impact(
        &self,
        actor: &WriteActor,
        key: &SettingKey,
        requested: Option<Uuid>,
        candidate: &Value,
        limit: Option<usize>,
    ) -> Result<ImpactOutcome, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-impact:p1:inst-vw-imp-2
        let (target, _, root) = self.writer.target_for(actor, requested).await?;
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-impact:p1:inst-vw-imp-2
        let conn = self.db.conn().map_err(|e| conn_error(&e))?;
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-impact:p1:inst-vw-imp-3
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-impact:p1:inst-vw-imp-4
        let declaration = self
            .writer
            .declaration_for_write(&conn, actor, key, root)
            .await?;
        let report = self
            .writer
            .impact(&conn, &declaration, target, candidate, limit)
            .await?;
        Ok(ImpactOutcome {
            report,
            data_classification: declaration.data_classification,
        })
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-impact:p1:inst-vw-imp-4
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-impact:p1:inst-vw-imp-3
    }

    /// The effective value at a target after a change, for the response.
    ///
    /// # Errors
    /// A resolver failure.
    pub async fn effective_after(
        &self,
        key: &SettingKey,
        target: ScopeTarget,
    ) -> Result<Arc<EffectiveValue>, DomainError> {
        let conn = self.db.conn().map_err(|e| conn_error(&e))?;
        self.writer.resolver().resolve(&conn, key, target).await
    }
}

/// An impact report together with the classification that masks it.
#[derive(Debug, Clone)]
pub struct ImpactOutcome {
    /// The bounded walk.
    pub report: ImpactReport,
    /// `public`, `pii` or `secret`, from the declaration.
    pub data_classification: String,
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "value_writes_tests.rs"]
mod value_writes_tests;
