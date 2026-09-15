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
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use toolkit_db::{DBProvider, DbError};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::audit::{AuditOperation, AuditRecord, AuditSink, AuditValue};
use crate::domain::category::DomainVisibility;
use crate::domain::declaration::{Declaration, DeclarationRepository};
use crate::domain::error::DomainError;
use crate::domain::resolution::{EffectiveValue, ScopeTarget};
use crate::domain::secrets::pending::{
    self as pending, Claim, PENDING_SECRET_TTL, PendingSecret, PendingSecretDraft,
    PendingSecretRepository,
};
use crate::domain::writes::service::{ImpactReport, StepUpPolicy, ValidationReport};
use crate::domain::writes::{Change, Committed, Gated, Staged, ValueWriter, WriteActor};
use crate::field;
use crate::infra::storage::access_repo::AccessRepo;
use crate::infra::storage::audit_store::AuditStore;
use crate::infra::storage::declaration_repo::DeclarationRepo;
use crate::infra::storage::pending_secret_repo::PendingSecretRepo;
use crate::infra::storage::value_repo::ValueRepo;

/// The writer over the concrete repositories and the audit store.
pub type ConcreteWriter = ValueWriter<DeclarationRepo, ValueRepo, AccessRepo, AuditStore>;

/// The most changes one batch may carry.
pub const BATCH_LIMIT: usize = 500;

/// How many expired stages one sweep pass releases at most.
pub const SWEEP_LIMIT: u64 = 200;

/// The batch operation that stores a value — the default, so a client that
/// never heard of the field keeps working unchanged.
pub const OP_SET: &str = "set";

/// The batch operation that clears the scope's own override.
pub const OP_REVERT: &str = "revert";

/// One change of a batch.
///
/// The operation stays the word the request used until the entry is evaluated.
/// A batch entry stands or falls alone, so an unrecognised operation — or a
/// value that contradicts the one named — has to refuse that entry with
/// `invalid` and leave the rest of the batch to commit; parsing it here, where
/// the whole request is still one value, could only refuse all of them.
#[derive(Debug, Clone)]
pub struct BatchChange {
    /// The setting.
    pub key: SettingKey,
    /// The target tenant; absent, the caller's own.
    pub tenant: Option<Uuid>,
    /// [`OP_SET`] or [`OP_REVERT`]; absent is [`OP_SET`].
    pub op: Option<String>,
    /// The value. Carried by a set, absent from a revert.
    pub value: Option<Value>,
    /// The tag the caller last read.
    pub if_match: Option<String>,
}

/// The change an entry asks for, before a secret's staged entry is resolved.
///
/// `set` and `revert` are the whole vocabulary: `remove` is the administrative
/// deletion of a row and is not offered here, and `AdoptSecret` is not asked
/// for by name — it is what a set of a `pending_id` becomes.
fn requested_change(op: Option<&str>, value: Option<Value>) -> Result<Change, DomainError> {
    let invalid = |field: &str, message: String| DomainError::Validation {
        field: field.to_owned(),
        code: field::VALIDATION,
        message,
    };
    match op.unwrap_or(OP_SET) {
        OP_SET => value
            .map(Change::Set)
            .ok_or_else(|| invalid("value", format!("a `{OP_SET}` change carries a value"))),
        OP_REVERT if value.is_some() => Err(invalid(
            "value",
            format!("a `{OP_REVERT}` change carries no value"),
        )),
        OP_REVERT => Ok(Change::Revert),
        other => Err(invalid(
            "op",
            format!("unknown operation `{other}`; one of `{OP_SET}` or `{OP_REVERT}`"),
        )),
    }
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
    pending: PendingSecretRepo,
    declarations: DeclarationRepo,
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
        Self {
            db,
            writer,
            pending: PendingSecretRepo,
            declarations: DeclarationRepo,
        }
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
            let BatchChange {
                key,
                tenant,
                op,
                value,
                if_match,
            } = change;
            let gated = self
                .writer
                .gate(
                    &conn,
                    actor,
                    &key,
                    tenant,
                    StepUpPolicy::AlreadyVerified,
                    "batch",
                )
                .await;
            let outcome = match gated {
                Ok(gated) => match self
                    .batch_change_for(&conn, &gated, actor, op.as_deref(), value)
                    .await
                {
                    Ok(change) => {
                        self.commit(actor, gated, change, if_match.as_deref(), change_set_id)
                            .await
                    }
                    Err(err) => {
                        self.writer
                            .after_rejection(&gated.declaration.key, gated.tenant_id, actor, &err)
                            .await;
                        Err(err)
                    }
                },
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

    /// The change a batch entry carries: the operation it names, and for a set
    /// the value — or, for a secret setting, a `pending_id` standing in for
    /// one, resolved to the entry staged earlier. Any other shape is a value
    /// and validates as one.
    ///
    /// Every refusal here is this entry's alone: the caller records it as a
    /// rejection and goes on to the next change.
    async fn batch_change_for<C: DBRunner>(
        &self,
        conn: &C,
        gated: &Gated,
        actor: &WriteActor,
        op: Option<&str>,
        value: Option<Value>,
    ) -> Result<Change, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-10
        // A revert takes the same route a set does — the gate, the `If-Match`
        // check and the commit are one implementation — and parts from it only
        // in the change it hands on, which the shared commit already knows how
        // to apply and which audit records as a revert.
        let value = match requested_change(op, value)? {
            Change::Set(value) => value,
            // `requested_change` yields a set or a revert and nothing else.
            other => return Ok(other),
        };
        // @cpt-end:cpt-cf-settings-service-flow-value-writes-batch:p1:inst-vw-batch-10
        if gated.declaration.has_secret_trait
            && let Some(pending_id) = pending::pending_id_of(&value)
        {
            return self
                .claim_pending(conn, gated, actor, pending_id)
                .await
                .map(Change::AdoptSecret);
        }
        Ok(Change::Set(value))
    }

    /// Claim a staged secret for a batch change: the row must exist, be
    /// unexpired, and name this change's declaration, tenant and subject.
    /// Single-use, consumed here before the commit: a commit that then fails
    /// releases the entry through the ordinary discard path, so no token is
    /// ever left pointing at nothing.
    async fn claim_pending<C: DBRunner>(
        &self,
        conn: &C,
        gated: &Gated,
        actor: &WriteActor,
        pending_id: Uuid,
    ) -> Result<String, DomainError> {
        let scope = AccessScope::allow_all();
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-7
        let Some(row) = self.pending.find(conn, &scope, pending_id).await? else {
            return Err(pending::invalid_pending());
        };
        let subject = actor.subject();
        pending::check_claim(
            &row,
            &Claim {
                declaration_id: gated.declaration.id,
                tenant_id: gated.tenant_id,
                subject_id: &subject,
                now: OffsetDateTime::now_utc(),
            },
        )?;
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-7
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-8
        // The row goes before the commit, whatever the commit does: a token is
        // claimed once. A delete that finds nothing lost the race to another
        // claim of the same token.
        if !self.pending.delete(conn, &scope, pending_id).await? {
            return Err(pending::invalid_pending());
        }
        Ok(row.secret_ref)
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-8
    }

    /// Stage a `secret`-trait value ahead of step-up: validate it, put the
    /// plaintext in the store under this gear's principal exactly as a set
    /// would, and hand back a token the batch names in place of the value.
    /// No step-up is asked, since nothing live changes here.
    ///
    /// # Errors
    /// The gate's refusals short of step-up; [`DomainError::Validation`] with
    /// `not_a_secret` for a declaration whose values travel inline, or for an
    /// invalid value; [`DomainError::Unavailable`] when the store or the
    /// database cannot answer, in which case nothing is kept anywhere.
    pub async fn stage_secret(
        &self,
        actor: &WriteActor,
        key: &SettingKey,
        requested: Option<Uuid>,
        value: Value,
    ) -> Result<PendingSecret, DomainError> {
        let conn = self.db.conn().map_err(|e| conn_error(&e))?;
        // A stage is also the moment to let go of what nobody claimed; a sweep
        // that fails is logged and tried again by the next one.
        if let Err(err) = self.sweep_expired_in(&conn, SWEEP_LIMIT).await {
            tracing::warn!(%err, "pending-secret sweep failed; expired stages wait for the next pass");
        }
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-1
        // The value write gate with step-up skipped: authorization was decided
        // by the caller, and a service principal is still refused a setting
        // that requires a person.
        let gated = self
            .writer
            .gate(
                &conn,
                actor,
                key,
                requested,
                StepUpPolicy::AlreadyVerified,
                "stage",
            )
            .await?;
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-1
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-2
        if !gated.declaration.has_secret_trait {
            return Err(DomainError::Validation {
                field: "key".to_owned(),
                code: field::NOT_A_SECRET,
                message: format!(
                    "`{key}` is not a secret setting: its values travel inline and need no staging"
                ),
            });
        }
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-2
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-3
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-4
        // Exactly the set's own staging: validated against the type, then into
        // the store under a reference unique to this stage; a store that
        // cannot answer refuses here and nothing is kept.
        let staged = self.writer.stage(&gated, Change::Set(value)).await?;
        let Staged::Set {
            secret_ref: Some(secret_ref),
            ..
        } = &staged
        else {
            return Err(DomainError::Internal {
                diagnostic: "staging a secret produced no store reference".to_owned(),
            });
        };
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-4
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-3
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-5
        // The row and its record in one transaction; a transaction that does
        // not commit releases the entry, as a refused set does.
        let draft = PendingSecretDraft {
            declaration_id: gated.declaration.id,
            tenant_id: gated.tenant_id,
            subject_id: actor.subject(),
            secret_ref: secret_ref.clone(),
            expires_at: OffsetDateTime::now_utc() + PENDING_SECRET_TTL,
        };
        let pending = self.pending;
        let key_owned = gated.declaration.key.clone();
        let tenant_id = gated.tenant_id;
        let actor_owned = actor.clone();
        let outcome = self
            .db
            .db()
            .transaction_ref_mapped::<_, PendingSecret, DomainError>(move |tx| {
                Box::pin(async move {
                    let scope = AccessScope::allow_all();
                    let row = pending.insert(tx, &scope, draft).await?;
                    let record = AuditRecord::new(
                        key_owned.as_str(),
                        Some(tenant_id),
                        actor_owned.subject(),
                        AuditOperation::Stage,
                        actor_owned.request_id.clone(),
                    )
                    .with_post_image(AuditValue::Masked);
                    AuditStore.append(tx, &scope, record).await?;
                    Ok(row)
                })
            })
            .await;
        match outcome {
            Ok(row) => Ok(row),
            Err(err) => {
                self.writer.discard(&gated, &staged).await;
                Err(err)
            }
        }
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-5
    }

    /// Release the stages nobody claimed: each expired row is deleted and its
    /// entry released, together. Returns how many rows went.
    ///
    /// # Errors
    /// [`DomainError`] when the database cannot list or delete; a store that
    /// cannot release is logged per entry, the row already gone.
    pub async fn sweep_expired(&self, limit: u64) -> Result<usize, DomainError> {
        let conn = self.db.conn().map_err(|e| conn_error(&e))?;
        self.sweep_expired_in(&conn, limit).await
    }

    async fn sweep_expired_in<C: DBRunner>(
        &self,
        conn: &C,
        limit: u64,
    ) -> Result<usize, DomainError> {
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-9
        let scope = AccessScope::allow_all();
        let expired = self
            .pending
            .list_expired(conn, &scope, OffsetDateTime::now_utc(), limit)
            .await?;
        let mut released = 0;
        for row in expired {
            // Gone already: a claim got there first, and the entry is its.
            if !self.pending.delete(conn, &scope, row.id).await? {
                continue;
            }
            released += 1;
            let key = self
                .declarations
                .find(
                    conn,
                    &scope,
                    &DomainVisibility::Unrestricted,
                    row.declaration_id,
                )
                .await?
                .map(|d| d.key)
                .unwrap_or_default();
            if let Err(err) = self
                .writer
                .secrets()
                .delete_secret(&key, row.tenant_id, &row.secret_ref)
                .await
            {
                tracing::warn!(
                    pending_id = %row.id,
                    tenant = %row.tenant_id,
                    %err,
                    "expired staged secret not released from the store; the reference is orphaned"
                );
            }
        }
        Ok(released)
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-9
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
