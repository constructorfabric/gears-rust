//! Atomic idempotency and coalescing. One slot can hold an active run and at
//! most one parked successor, so a refresh during execution is never lost.
use super::{
    JournalStore, StoreError, coalescing, normalized,
    repository::{enqueue, status},
    run, start_key,
};
use crate::domain::error::DomainError;
use crate::domain::journal::Journal;
use aws_lc_rs::digest::{SHA256, digest};
use chrono::Utc;
use durable_execution_sdk::{RunId, StartOptions, StartResult, reason};
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait, sea_query::Expr};
use toolkit_db::secure::{DBRunner, SecureEntityExt, SecureUpdateExt, secure_insert};
use toolkit_security::AccessScope;
use uuid::Uuid;

// Resolve old physical IDs by the scoped logical key before any fenced update.
async fn coalescing_slot<C: DBRunner>(
    conn: &C,
    scope: &AccessScope,
    candidate: &Journal,
    key_hash: &str,
) -> Result<Option<coalescing::Model>, StoreError> {
    Ok(coalescing::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(
            Condition::all()
                .add(coalescing::Column::TenantId.eq(candidate.run.owner.tenant_id))
                .add(coalescing::Column::OwnerId.eq(candidate.run.owner.subject_id))
                .add(coalescing::Column::Definition.eq(&candidate.run.definition))
                .add(coalescing::Column::KeyHash.eq(key_hash)),
        )
        .one(conn)
        .await?)
}
impl StoreError {
    pub(crate) fn retryable_race(&self) -> bool {
        matches!(self, Self::Conflict) || matches!(self, Self::Scope(e) if e.is_unique_violation())
    }
}
impl JournalStore {
    pub async fn start(
        &self,
        scope: AccessScope,
        candidate: Journal,
        options: StartOptions,
    ) -> Result<StartResult, DomainError> {
        if options
            .expected_fingerprint
            .as_ref()
            .is_some_and(|expected| expected != &candidate.fingerprint)
        {
            return Err(DomainError::DefinitionMismatch);
        }
        for (field, key) in [
            ("idempotency_key", &options.idempotency_key),
            ("coalescing_key", &options.coalescing_key),
        ] {
            if key.as_ref().is_some_and(|k| k.is_empty() || k.len() > 256) {
                return Err(DomainError::InvalidRequest {
                    field,
                    reason: reason::OUT_OF_RANGE,
                    message: "keys must contain 1..=256 bytes",
                });
            }
        }
        #[cfg(test)]
        if let Some(contract) = candidate.registration_contract.clone() {
            self.ensure_contract(contract).await?;
        }
        for _ in 0..8 {
            match self
                .start_once(scope.clone(), candidate.clone(), options.clone())
                .await
            {
                Err(e) if e.retryable_race() => tokio::task::yield_now().await,
                result => return result.map_err(DomainError::from),
            }
        }
        Err(DomainError::ConcurrentUpdate)
    }
    async fn start_once(
        &self,
        scope: AccessScope,
        mut candidate: Journal,
        options: StartOptions,
    ) -> Result<StartResult, StoreError> {
        let publication = std::sync::Arc::new(super::repository::Publication::new(
            self.publisher.read().clone(),
        ));
        let publisher = publication.clone();
        let catalog_scope = self.worker_scope("definition:get").await?;
        let postgres = self.postgres();
        self.db
            .db()
            .transaction_ref_mapped(move |txn| {
                Box::pin(async move {
                    let hashes = InputHashes::new(&candidate.input)?;
                    let input_hash = hashes.canonical.clone();
                    let key_id = options
                        .idempotency_key
                        .as_deref()
                        .map(|key| scoped_id(&candidate, "request", key));
                    if let Some(previous) = replay(txn, &scope, key_id, &hashes).await? {
                        return Ok(previous);
                    }
                    super::catalog::admit(txn, &catalog_scope, &mut candidate, postgres).await?;
                    let mut target = candidate.run.id;
                    let mut generation = 1i64;
                    let mut insert = true;
                    let mut coalesced = false;
                    if let Some(key) = options.coalescing_key.as_deref() {
                        let slot_id = scoped_id(&candidate, "coalesce", key);
                        let key_hash = coalescing_key_hash(&candidate, key);
                        let previous = coalescing_slot(txn, &scope, &candidate, &key_hash).await?;
                        if let Some(slot) = previous {
                            if !hashes
                                .matches(
                                    txn,
                                    &scope,
                                    &slot.input_hash,
                                    slot.active_run_id.or(slot.successor_run_id),
                                )
                                .await?
                            {
                                return Err(StoreError::RequestConflict("coalescing_key"));
                            }
                            let active = match slot.active_run_id {
                                Some(id) => read(txn, &scope, id).await?,
                                None => None,
                            };
                            let mut active_id = slot.active_run_id;
                            let mut successor_id = slot.successor_run_id;
                            generation = slot.requested_generation;
                            if let Some(successor) = successor_id {
                                target = RunId(successor);
                                insert = false;
                                coalesced = true;
                            } else if let Some(mut active) =
                                active.filter(|a| !a.run.status.is_terminal())
                            {
                                if active.run.activities.iter().all(|step| step.attempts == 0) {
                                    // Serialize with a worker claiming the previously queued run.
                                    // If its revision changed, retry and allocate a successor.
                                    let revision = active.revision;
                                    active.revision =
                                        revision.checked_add(1).ok_or(StoreError::OUT_OF_RANGE)?;
                                    let changed = run::Entity::update_many()
                                        .secure()
                                        .scope_with(&scope)
                                        .filter(
                                            Condition::all()
                                                .add(run::Column::Id.eq(active.run.id.0))
                                                .add(run::Column::Revision.eq(revision)),
                                        )
                                        .col_expr(
                                            run::Column::Revision,
                                            Expr::value(active.revision),
                                        )
                                        .col_expr(
                                            run::Column::Journal,
                                            Expr::value(normalized::metadata(&active)?),
                                        )
                                        .col_expr(run::Column::StorageVersion, Expr::value(1))
                                        .col_expr(
                                            run::Column::ActivityCount,
                                            Expr::value(
                                                i32::try_from(active.run.activities.len())
                                                    .map_err(|_| {
                                                        StoreError::Invariant(
                                                            "activity count out of range",
                                                        )
                                                    })?,
                                            ),
                                        )
                                        .exec(txn)
                                        .await?;
                                    if changed.rows_affected != 1 {
                                        return Err(StoreError::Conflict);
                                    }
                                    normalized::persist(txn, &scope, &active).await?;
                                    target = active.run.id;
                                    insert = false;
                                    coalesced = true;
                                } else {
                                    // This input arrived after execution began. A parked
                                    // successor is activated by reconciliation after it ends.
                                    successor_id = Some(target.0);
                                    candidate.run.next_attempt_at = None;
                                    generation = generation
                                        .checked_add(1)
                                        .ok_or(StoreError::OUT_OF_RANGE)?;
                                }
                            } else {
                                active_id = Some(target.0);
                                generation =
                                    generation.checked_add(1).ok_or(StoreError::OUT_OF_RANGE)?;
                            }
                            if insert {
                                insert_run(txn, &scope, &candidate).await?;
                            }
                            // CAS even when reusing a run: serialize with promotion and
                            // concurrent starts across nodes without holding network I/O.
                            let changed = coalescing::Entity::update_many()
                                .secure()
                                .scope_with(&scope)
                                .filter(
                                    Condition::all()
                                        .add(coalescing::Column::Id.eq(slot.id))
                                        .add(coalescing::Column::Revision.eq(slot.revision)),
                                )
                                .col_expr(
                                    coalescing::Column::Revision,
                                    Expr::value(slot.revision + 1),
                                )
                                .col_expr(
                                    coalescing::Column::InputHash,
                                    Expr::value(input_hash.clone()),
                                )
                                .col_expr(coalescing::Column::ActiveRunId, Expr::value(active_id))
                                .col_expr(
                                    coalescing::Column::SuccessorRunId,
                                    Expr::value(successor_id),
                                )
                                .col_expr(
                                    coalescing::Column::RequestedGeneration,
                                    Expr::value(generation),
                                )
                                .exec(txn)
                                .await?;
                            if changed.rows_affected != 1 {
                                return Err(StoreError::Conflict);
                            }
                        } else {
                            insert_run(txn, &scope, &candidate).await?;
                            secure_insert::<coalescing::Entity>(
                                coalescing::ActiveModel {
                                    id: Set(slot_id),
                                    tenant_id: Set(candidate.run.owner.tenant_id),
                                    owner_id: Set(candidate.run.owner.subject_id),
                                    definition: Set(candidate.run.definition.clone()),
                                    key_hash: Set(key_hash),
                                    input_hash: Set(input_hash.clone()),
                                    revision: Set(0),

                                    active_run_id: Set(Some(target.0)),
                                    successor_run_id: Set(None),
                                    requested_generation: Set(generation),
                                },
                                &scope,
                                txn,
                            )
                            .await?;
                        }
                    } else {
                        insert_run(txn, &scope, &candidate).await?;
                    }
                    if insert && candidate.run.next_attempt_at.is_some() {
                        enqueue(txn, &scope, &candidate, &publisher).await?;
                    }
                    if let Some(id) = key_id {
                        secure_insert::<start_key::Entity>(
                            start_key::ActiveModel {
                                id: Set(id),
                                tenant_id: Set(candidate.run.owner.tenant_id),
                                owner_id: Set(candidate.run.owner.subject_id),
                                definition: Set(candidate.run.definition),
                                key_hash: Set(hash(
                                    options.idempotency_key.as_deref().unwrap_or_default(),
                                )),
                                input_hash: Set(input_hash),
                                run_id: Set(target.0),
                                generation: Set(generation),
                            },
                            &scope,
                            txn,
                        )
                        .await?;
                    }
                    Ok(StartResult {
                        run_id: target,
                        requested_generation: u64::try_from(generation)
                            .map_err(|_| StoreError::OUT_OF_RANGE)?,
                        coalesced,
                    })
                })
            })
            .await
            .inspect(|_value| {
                publication.fire();
            })
    }
    pub async fn promote_successors(&self) -> Result<(), StoreError> {
        let scope = self.worker_scope("recover").await?;
        let conn = self.db.conn()?;
        let slots = coalescing::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(coalescing::Column::SuccessorRunId.is_not_null()))
            .all(&conn)
            .await?;
        let catalog_scope = self.worker_scope("definition:get").await?;
        let postgres = self.postgres();
        for slot in slots {
            let catalog_scope = catalog_scope.clone();
            let scope = self.worker_scope("recover").await?;
            let publication = std::sync::Arc::new(super::repository::Publication::new(
                self.publisher.read().clone(),
            ));
            let publisher = publication.clone();
            let result = self
                .db
                .db()
                .transaction_ref_mapped(move |txn| {
                    Box::pin(async move {
                        if let Some(active) = slot.active_run_id
                            && let Some(run) = read(txn, &scope, active).await?
                            && !run.run.status.is_terminal()
                        {
                            return Ok::<(), StoreError>(());
                        }
                        let Some(id) = slot.successor_run_id else {
                            return Ok(());
                        };
                        let Some(mut successor) = read(txn, &scope, id).await? else {
                            return Err(StoreError::Conflict);
                        };
                        let registration = super::catalog::locked(
                            txn,
                            &catalog_scope,
                            &successor.run.definition,
                            false,
                            postgres,
                        )
                        .await?;
                        if registration
                            .is_some_and(|r| !r.permits(successor.registration_generation))
                        {
                            return Ok(());
                        }
                        let next_id = if successor.run.status.is_terminal() {
                            None
                        } else {
                            Some(id)
                        };
                        let changed = coalescing::Entity::update_many()
                            .secure()
                            .scope_with(&scope)
                            .filter(
                                Condition::all()
                                    .add(coalescing::Column::Id.eq(slot.id))
                                    .add(coalescing::Column::Revision.eq(slot.revision)),
                            )
                            .col_expr(coalescing::Column::Revision, Expr::value(slot.revision + 1))
                            .col_expr(coalescing::Column::ActiveRunId, Expr::value(next_id))
                            .col_expr(
                                coalescing::Column::SuccessorRunId,
                                Expr::value(Option::<Uuid>::None),
                            )
                            .exec(txn)
                            .await?;
                        if changed.rows_affected != 1 {
                            return Err(StoreError::Conflict);
                        }
                        if next_id.is_some() {
                            let revision = successor.revision;
                            successor.revision += 1;
                            successor.run.next_attempt_at = Some(Utc::now());
                            successor.run.updated_at = Utc::now();
                            let changed = run::Entity::update_many()
                                .secure()
                                .scope_with(&scope)
                                .filter(
                                    Condition::all()
                                        .add(run::Column::Id.eq(id))
                                        .add(run::Column::Revision.eq(revision)),
                                )
                                .col_expr(run::Column::Revision, Expr::value(successor.revision))
                                .col_expr(
                                    run::Column::Journal,
                                    Expr::value(normalized::metadata(&successor)?),
                                )
                                .col_expr(
                                    run::Column::DueAt,
                                    Expr::value(successor.run.next_attempt_at),
                                )
                                .col_expr(
                                    run::Column::UpdatedAt,
                                    Expr::value(successor.run.updated_at),
                                )
                                .col_expr(run::Column::StorageVersion, Expr::value(1))
                                .col_expr(
                                    run::Column::ActivityCount,
                                    Expr::value(
                                        i32::try_from(successor.run.activities.len())
                                            .map_err(|_| StoreError::OUT_OF_RANGE)?,
                                    ),
                                )
                                .exec(txn)
                                .await?;
                            if changed.rows_affected != 1 {
                                return Err(StoreError::Conflict);
                            }
                            normalized::persist(txn, &scope, &successor).await?;
                            enqueue(txn, &scope, &successor, &publisher).await?;
                        }
                        Ok(())
                    })
                })
                .await
                .inspect(|_value| {
                    publication.fire();
                });
            if !matches!(result, Err(StoreError::Conflict)) {
                result?;
            }
        }
        Ok(())
    }
}
async fn read<C: DBRunner>(
    txn: &C,
    scope: &AccessScope,
    id: Uuid,
) -> Result<Option<Journal>, StoreError> {
    let row = run::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(run::Column::Id.eq(id)))
        .one(txn)
        .await?;
    match row {
        Some(row) => normalized::hydrate(txn, scope, row).await.map(Some),
        None => Ok(None),
    }
}

async fn insert_run<C: DBRunner>(
    txn: &C,
    scope: &AccessScope,
    j: &Journal,
) -> Result<(), StoreError> {
    secure_insert::<run::Entity>(
        run::ActiveModel {
            id: Set(j.run.id.0),
            tenant_id: Set(j.run.owner.tenant_id),
            owner_id: Set(j.run.owner.subject_id),
            definition: Set(j.run.definition.clone()),
            revision: Set(0),
            registration_generation: Set(
                i64::try_from(j.registration_generation).map_err(|_| StoreError::OUT_OF_RANGE)?
            ),
            status: Set(status(j.run.status).into()),
            journal: Set(normalized::metadata(j)?),
            storage_version: Set(1),
            activity_count: Set(
                i32::try_from(j.run.activities.len()).map_err(|_| StoreError::OUT_OF_RANGE)?
            ),
            lease_until: Set(None),
            due_at: Set(j.run.next_attempt_at),
            created_at: Set(j.run.created_at),
            updated_at: Set(j.run.updated_at),
        },
        scope,
        txn,
    )
    .await?;
    normalized::persist(txn, scope, j).await?;
    super::events::record(txn, scope, None, j).await?;
    Ok(())
}
fn hash(key: &str) -> String {
    hex::encode(digest(&SHA256, key.as_bytes()).as_ref())
}
fn coalescing_key_hash(j: &Journal, key: &str) -> String {
    if j.registration_generation == 0 {
        hash(key)
    } else {
        format!("{}:{}", j.registration_generation, hash(key))
    }
}
fn scoped_id(j: &Journal, kind: &str, key: &str) -> Uuid {
    if kind == "coalesce" && j.registration_generation > 0 {
        // Generation-zero keys accept arbitrary suffixes. A separate namespace
        // prevents those suffixes from becoming another generation's framing.
        let namespace = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"durable-coalescing-generations-v1");
        let text = format!(
            "{}:{}:{}:{}:{}:{}",
            j.run.owner.tenant_id,
            j.run.owner.subject_id,
            j.run.definition,
            j.registration_generation,
            key.len(),
            key
        );
        return Uuid::new_v5(&namespace, text.as_bytes());
    }
    let text = format!(
        "{}:{}:{}:{}:{}",
        j.run.owner.tenant_id, j.run.owner.subject_id, j.run.definition, kind, key
    );
    Uuid::new_v5(&Uuid::NAMESPACE_OID, text.as_bytes())
}

struct InputHashes {
    canonical: String,
    encoded: String,
}
impl InputHashes {
    fn new(input: &serde_json::Value) -> Result<Self, StoreError> {
        let encoded = hex::encode(digest(&SHA256, &serde_json::to_vec(input)?).as_ref());
        let mut canonical = input.clone();
        canonical.sort_all_objects();
        let canonical = hex::encode(digest(&SHA256, &serde_json::to_vec(&canonical)?).as_ref());
        Ok(Self { canonical, encoded })
    }
    async fn matches<C: DBRunner>(
        &self,
        txn: &C,
        scope: &AccessScope,
        stored: &str,
        run_id: Option<Uuid>,
    ) -> Result<bool, StoreError> {
        if stored == self.canonical || stored == self.encoded {
            return Ok(true);
        }
        // Old hosts may have hashed insertion order. Compare their persisted
        // input under the caller's scope instead of invalidating that request.
        let Some(id) = run_id else {
            return Ok(false);
        };
        let Some(journal) = read(txn, scope, id).await? else {
            return Ok(false);
        };
        Ok(Self::new(&journal.input)?.canonical == self.canonical)
    }
}

async fn replay<C: DBRunner>(
    txn: &C,
    scope: &AccessScope,
    key: Option<Uuid>,
    hashes: &InputHashes,
) -> Result<Option<StartResult>, StoreError> {
    let Some(key) = key else {
        return Ok(None);
    };
    let Some(previous) = start_key::Entity::find()
        .secure()
        .scope_with(scope)
        .filter(Condition::all().add(start_key::Column::Id.eq(key)))
        .one(txn)
        .await?
    else {
        return Ok(None);
    };
    if !hashes
        .matches(txn, scope, &previous.input_hash, Some(previous.run_id))
        .await?
    {
        return Err(StoreError::RequestConflict("idempotency_key"));
    }
    Ok(Some(StartResult {
        run_id: RunId(previous.run_id),
        requested_generation: u64::try_from(previous.generation)
            .map_err(|_| StoreError::OUT_OF_RANGE)?,
        coalesced: false,
    }))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../../tests/unit/start_contract_tests.rs"]
mod contract_tests;
