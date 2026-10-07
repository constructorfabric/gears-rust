use super::{normalized, outbox, run};
use crate::domain::persisted::RunStatus;
use crate::domain::{error::DomainError, journal::Journal};
use chrono::{DateTime, Utc};
use durable_execution_sdk::RunId;
use sea_orm::{
    ActiveValue::Set, ColumnTrait, Condition, EntityTrait, QuerySelect, sea_query::Expr,
};
use std::sync::Arc;
use toolkit_db::{
    DBProvider, DbError,
    secure::{DBRunner, SecureEntityExt, SecureUpdateExt, secure_insert},
};
use toolkit_security::AccessScope;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("journal scope operation failed")]
    Scope(#[from] toolkit_db::secure::ScopeError),
    #[error("journal database operation failed")]
    Database(#[from] DbError),
    #[error("journal serialization failed")]
    Codec(#[from] serde_json::Error),
    #[error("outbox publication failed")]
    Outbox(#[from] toolkit_db::outbox::OutboxError),
    #[error("delivery pipeline is unavailable")]
    DeliveryUnavailable,
    #[error("worker policy authorization failed")]
    Authorization(#[source] Box<authz_resolver_sdk::pep::EnforcerError>),
    #[error("worker authorization is unavailable")]
    AuthorizationUnavailable,
    /// A compare-and-swap lost a race or observed a torn read; retryable.
    #[error("journal conflict")]
    Conflict,
    /// Persisted state violates an invariant (overflow, corrupt rows).
    #[error("journal invariant violated: {0}")]
    Invariant(&'static str),
    #[error("definition management access denied")]
    Forbidden,
    #[error("definition is inactive")]
    DefinitionInactive,
    /// The named idempotency or coalescing key was reused with different input.
    #[error("{0} input conflict")]
    RequestConflict(&'static str),
    #[error(transparent)]
    Domain(#[from] DomainError),
}
impl StoreError {
    /// A persisted counter or conversion left its numeric range.
    pub(crate) const OUT_OF_RANGE: Self = Self::Invariant("numeric value out of range");
}
impl From<sea_orm::DbErr> for StoreError {
    fn from(e: sea_orm::DbErr) -> Self {
        Self::Database(DbError::from(e))
    }
}
impl From<StoreError> for DomainError {
    // A flat match is the whole mapping; the structured `tracing` calls only
    // count toward complexity, and helpers would hide which source is logged.
    #[expect(
        clippy::cognitive_complexity,
        reason = "Keep each store failure classification and its safe diagnostic together in the boundary mapping."
    )]
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::Domain(e) => e,
            StoreError::Forbidden => Self::Forbidden,
            StoreError::Authorization(e)
                if matches!(*e, authz_resolver_sdk::pep::EnforcerError::Denied { .. }) =>
            {
                Self::Forbidden
            }
            StoreError::DefinitionInactive => Self::DefinitionInactive,
            StoreError::Conflict => Self::ConcurrentUpdate,
            StoreError::RequestConflict(key) => Self::IdempotencyConflict(key),
            StoreError::Invariant(what) => Self::Internal(what),
            StoreError::Codec(e) => {
                tracing::error!(error = %e, "durable journal serialization failed");
                Self::Internal("journal serialization failed")
            }
            StoreError::Scope(e) => {
                tracing::error!(error = %e, "durable journal scope operation failed");
                Self::Internal("journal scope operation failed")
            }
            e @ (StoreError::Database(_)
            | StoreError::Outbox(_)
            | StoreError::DeliveryUnavailable
            | StoreError::Authorization(_)
            | StoreError::AuthorizationUnavailable) => {
                tracing::warn!(error = %e, "durable execution dependency unavailable");
                Self::Unavailable
            }
        }
    }
}
#[cfg(test)]
#[derive(Clone, Debug)]
pub struct Delivery {
    pub id: Uuid,
    pub run_id: RunId,
    pub generation: i64,
}

/// Callers supply scopes derived from the policy layer; background control-plane
/// scans explicitly use the runtime's service scope, never an HTTP input scope.
#[derive(Clone)]
pub struct JournalStore {
    #[cfg(all(test, feature = "integration"))]
    pub(crate) test_database: Option<Arc<dyn Send + Sync>>,
    authorization: Option<Arc<dyn crate::infra::authorization::WorkerAuthorization>>,
    pub(super) db: Arc<DBProvider<DbError>>,
    pub(super) publisher: Arc<parking_lot::RwLock<Option<Arc<toolkit_db::outbox::Outbox>>>>,
}
#[derive(sea_orm::FromQueryResult)]
struct DueRun {
    id: Uuid,
    due_at: DateTime<Utc>,
}
impl JournalStore {
    #[must_use]
    pub fn new(db: Arc<DBProvider<DbError>>) -> Self {
        Self {
            #[cfg(all(test, feature = "integration"))]
            test_database: None,
            authorization: None,
            db,
            publisher: Arc::default(),
        }
    }
    pub(crate) fn with_authorization(
        mut self,
        authorization: Arc<dyn crate::infra::authorization::WorkerAuthorization>,
    ) -> Self {
        self.authorization = Some(authorization);
        self
    }
    pub(crate) async fn worker_scope(&self, action: &str) -> Result<AccessScope, StoreError> {
        if let Some(authorization) = &self.authorization {
            return authorization.scope(action).await;
        }
        Err(StoreError::AuthorizationUnavailable)
    }
    pub async fn start_outbox(
        &self,
        handler: impl toolkit_db::outbox::LeasedMessageHandler + 'static,
    ) -> anyhow::Result<toolkit_db::outbox::OutboxHandle> {
        let handle = crate::infra::outbox::start(self.db.db().clone(), handler).await?;
        *self.publisher.write() = Some(handle.outbox().clone());
        Ok(handle)
    }
    #[cfg(test)]
    #[cfg_attr(coverage_nightly, coverage(off))]
    pub async fn insert(&self, scope: AccessScope, journal: Journal) -> Result<(), StoreError> {
        if let Some(contract) = journal.registration_contract.clone() {
            self.ensure_contract_with_scope(contract, scope.clone())
                .await?;
        }
        let publication = std::sync::Arc::new(super::repository::Publication::new(
            self.publisher.read().clone(),
        ));
        let publisher = publication.clone();
        self.db
            .db()
            .transaction_ref_mapped(move |txn| {
                Box::pin(async move {
                    secure_insert::<run::Entity>(
                        run::ActiveModel {
                            id: Set(journal.run.id.0),
                            tenant_id: Set(journal.run.owner.tenant_id),
                            owner_id: Set(journal.run.owner.subject_id),
                            definition: Set(journal.run.definition.clone()),
                            revision: Set(0),
                            registration_generation: Set(i64::try_from(
                                journal.registration_generation,
                            )
                            .map_err(|_| {
                                StoreError::Invariant("registration generation out of range")
                            })?),
                            status: Set(status(journal.run.status).into()),
                            journal: Set(normalized::metadata(&journal)?),
                            storage_version: Set(1),
                            activity_count: Set(i32::try_from(journal.run.activities.len())
                                .map_err(|_| {
                                    StoreError::Invariant("activity count out of range")
                                })?),
                            lease_until: Set(journal.lease_until),
                            due_at: Set(journal.run.next_attempt_at),
                            created_at: Set(journal.run.created_at),
                            updated_at: Set(journal.run.updated_at),
                        },
                        &scope,
                        txn,
                    )
                    .await?;
                    normalized::persist(txn, &scope, &journal).await?;
                    super::events::record(txn, &scope, None, &journal).await?;
                    enqueue(txn, &scope, &journal, &publisher).await
                })
            })
            .await
            .inspect(|_value| {
                publication.fire();
            })
    }
    pub async fn get(&self, scope: &AccessScope, id: RunId) -> Result<Option<Journal>, StoreError> {
        self.get_on(&self.db.conn()?, scope, id).await
    }
    pub(crate) async fn get_on<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: RunId,
    ) -> Result<Option<Journal>, StoreError> {
        for _ in 0..8 {
            let row = run::Entity::find()
                .secure()
                .scope_with(scope)
                .filter(Condition::all().add(run::Column::Id.eq(id.0)))
                .one(conn)
                .await?;
            let Some(row) = row else {
                return Ok(None);
            };
            match normalized::hydrate(conn, scope, row).await {
                Err(StoreError::Conflict) => tokio::task::yield_now().await,
                result => return result.map(Some),
            }
        }
        Err(StoreError::Conflict)
    }

    /// Persist the complete checkpoint and its next delivery in one transaction.
    /// The revision read before executing the transition is the CAS token. A
    /// heartbeat, cancel, or competing worker forces the caller to reload.
    pub async fn save(
        &self,
        scope: AccessScope,
        expected_revision: i64,
        mut journal: Journal,
        enqueue_next: bool,
    ) -> Result<(), StoreError> {
        let publication = std::sync::Arc::new(super::repository::Publication::new(
            self.publisher.read().clone(),
        ));
        let publisher = publication.clone();
        let catalog_scope = self.worker_scope("definition:get").await?;
        let postgres = self.postgres();
        journal.revision = expected_revision
            .checked_add(1)
            .ok_or(StoreError::Invariant("revision overflow"))?;
        self.db
            .db()
            .transaction_ref_mapped(move |txn| {
                Box::pin(async move {
                    let registration = super::catalog::locked(
                        txn,
                        &catalog_scope,
                        &journal.run.definition,
                        false,
                        postgres,
                    )
                    .await?;
                    let previous = run::Entity::find()
                        .secure()
                        .scope_with(&scope)
                        .filter(
                            Condition::all()
                                .add(run::Column::Id.eq(journal.run.id.0))
                                .add(run::Column::Revision.eq(expected_revision)),
                        )
                        .one(txn)
                        .await?
                        .ok_or(StoreError::Conflict)?;
                    let previous = normalized::hydrate(txn, &scope, previous).await?;
                    if previous.registration_generation != journal.registration_generation {
                        return Err(StoreError::Conflict);
                    }
                    if registration.is_some_and(|r| !r.permits(journal.registration_generation))
                        && !super::catalog::cancellation_only(&previous, &journal)
                    {
                        return Err(StoreError::DefinitionInactive);
                    }
                    let updated = run::Entity::update_many()
                        .secure()
                        .scope_with(&scope)
                        .filter(
                            Condition::all()
                                .add(run::Column::Id.eq(journal.run.id.0))
                                .add(run::Column::Revision.eq(expected_revision)),
                        )
                        .col_expr(run::Column::Revision, Expr::value(journal.revision))
                        .col_expr(run::Column::Status, Expr::value(status(journal.run.status)))
                        .col_expr(
                            run::Column::Journal,
                            Expr::value(normalized::metadata(&journal)?),
                        )
                        .col_expr(run::Column::StorageVersion, Expr::value(1))
                        .col_expr(
                            run::Column::ActivityCount,
                            Expr::value(i32::try_from(journal.run.activities.len()).map_err(
                                |_| StoreError::Invariant("activity count out of range"),
                            )?),
                        )
                        .col_expr(run::Column::LeaseUntil, Expr::value(journal.lease_until))
                        .col_expr(run::Column::DueAt, Expr::value(journal.run.next_attempt_at))
                        .col_expr(run::Column::UpdatedAt, Expr::value(journal.run.updated_at))
                        .exec(txn)
                        .await?;
                    if updated.rows_affected != 1 {
                        return Err(StoreError::Conflict);
                    }
                    normalized::persist(txn, &scope, &journal).await?;
                    super::events::record(txn, &scope, Some(&previous), &journal).await?;
                    if enqueue_next {
                        enqueue(txn, &scope, &journal, &publisher).await?;
                    }
                    Ok(())
                })
            })
            .await
            .inspect(|_value| {
                publication.fire();
            })
    }
    #[cfg(test)]
    #[cfg_attr(coverage_nightly, coverage(off))]
    pub async fn deliveries(
        &self,
        scope: &AccessScope,
        now: DateTime<Utc>,
        limit: u64,
    ) -> Result<Vec<Delivery>, StoreError> {
        let conn = self.db.conn()?;
        Ok(outbox::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(outbox::Column::DeliveredAt.is_null())
                    .add(outbox::Column::DueAt.lte(now)),
            )
            .order_by(outbox::Column::DueAt, sea_orm::Order::Asc)
            .limit(limit.min(1000))
            .all(&conn)
            .await?
            .into_iter()
            .map(|r| Delivery {
                id: r.id,
                run_id: RunId(r.run_id),
                generation: r.generation,
            })
            .collect())
    }
    /// Called after enqueue succeeds; a crash before this write duplicates a
    /// delivery and is handled by claim fencing instead of losing the run.
    pub async fn mark_delivered(
        &self,
        scope: &AccessScope,
        id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.mark_delivered_on(&self.db.conn()?, scope, id, now)
            .await
    }
    pub(crate) async fn mark_delivered_on<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let updated = outbox::Entity::update_many()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(outbox::Column::Id.eq(id))
                    .add(outbox::Column::DeliveredAt.is_null()),
            )
            .col_expr(outbox::Column::DeliveredAt, Expr::value(Some(now)))
            .exec(conn)
            .await?;
        if updated.rows_affected == 1 {
            return Ok(());
        }
        // Duplicate acknowledgements are successful only when the caller can
        // still see an already acknowledged intent. A scoped zero-row update
        // must not consume the shared Outbox command.
        let acknowledged = outbox::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(outbox::Column::Id.eq(id))
                    .add(outbox::Column::DeliveredAt.is_not_null()),
            )
            .one(conn)
            .await?;
        if acknowledged.is_some() {
            Ok(())
        } else {
            Err(StoreError::Conflict)
        }
    }
    #[cfg(test)]
    #[cfg_attr(coverage_nightly, coverage(off))]
    pub async fn expired_claims(
        &self,
        scope: &AccessScope,
        now: DateTime<Utc>,
    ) -> Result<Vec<Journal>, StoreError> {
        self.expired_claims_on(&self.db.conn()?, scope, now).await
    }
    #[cfg(test)]
    #[cfg_attr(coverage_nightly, coverage(off))]
    pub(crate) async fn expired_claims_on<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        now: DateTime<Utc>,
    ) -> Result<Vec<Journal>, StoreError> {
        self.expired_claims_page_on(conn, scope, now, None)
            .await
            .map(|(rows, _)| rows)
    }
    pub(crate) async fn expired_claims_page(
        &self,
        scope: &AccessScope,
        now: DateTime<Utc>,
        after: Option<(DateTime<Utc>, Uuid)>,
    ) -> Result<(Vec<Journal>, Option<(DateTime<Utc>, Uuid)>), StoreError> {
        self.expired_claims_page_on(&self.db.conn()?, scope, now, after)
            .await
    }
    async fn expired_claims_page_on<C: DBRunner>(
        &self,
        conn: &C,
        scope: &AccessScope,
        now: DateTime<Utc>,
        after: Option<(DateTime<Utc>, Uuid)>,
    ) -> Result<(Vec<Journal>, Option<(DateTime<Utc>, Uuid)>), StoreError> {
        let mut filter = Condition::all()
            .add(run::Column::Status.is_in(["running", "cancelling"]))
            .add(run::Column::LeaseUntil.lte(now));
        if let Some((lease, id)) = after {
            filter = filter.add(
                Condition::any().add(run::Column::LeaseUntil.gt(lease)).add(
                    Condition::all()
                        .add(run::Column::LeaseUntil.eq(lease))
                        .add(run::Column::Id.gt(id)),
                ),
            );
        }
        let rows = run::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(filter)
            .order_by(run::Column::LeaseUntil, sea_orm::Order::Asc)
            .order_by(run::Column::Id, sea_orm::Order::Asc)
            .limit(100)
            .all(conn)
            .await?;
        let cursor = if rows.len() == 100 {
            rows.last()
                .and_then(|row| row.lease_until.map(|lease| (lease, row.id)))
        } else {
            None
        };
        let mut journals = Vec::with_capacity(rows.len());
        for row in rows {
            if let Some(journal) = self.get_on(conn, scope, RunId(row.id)).await? {
                journals.push(journal);
            }
        }
        Ok((journals, cursor))
    }

    /// Re-open deliveries acknowledged by the transport but never claimed by
    /// an executor. Only the current generation of due, unowned work qualifies.
    /// A concurrent claim is harmless: its lease/fence rejects the duplicate.
    pub async fn recover_unclaimed_deliveries(
        &self,
        now: DateTime<Utc>,
        stale_before: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.recover_unclaimed_deliveries_for_definition(now, stale_before, None)
            .await
    }
    pub(crate) async fn recover_unclaimed_deliveries_for_definition(
        &self,
        now: DateTime<Utc>,
        stale_before: DateTime<Utc>,
        definition: Option<&str>,
    ) -> Result<(), StoreError> {
        let scope = self.worker_scope("dispatch").await?;
        let conn = self.db.conn()?;
        let mut cursor = None;
        loop {
            let mut filter = Condition::all()
                .add(run::Column::Status.is_in(["queued", "retry_wait", "running"]))
                .add(run::Column::DueAt.lte(now));
            if let Some(definition) = definition {
                filter = filter.add(run::Column::Definition.eq(definition));
            }
            if let Some((due, id)) = cursor {
                filter = filter.add(
                    Condition::any().add(run::Column::DueAt.gt(due)).add(
                        Condition::all()
                            .add(run::Column::DueAt.eq(due))
                            .add(run::Column::Id.gt(id)),
                    ),
                );
            }
            let runs = run::Entity::find()
                .secure()
                .scope_with(&scope)
                .filter(filter)
                .order_by(run::Column::DueAt, sea_orm::Order::Asc)
                .order_by(run::Column::Id, sea_orm::Order::Asc)
                .limit(1000)
                .project_all(&conn, |q| {
                    q.select_only()
                        .column(run::Column::Id)
                        .column(run::Column::DueAt)
                        .into_model::<DueRun>()
                })
                .await?;
            let Some(last) = runs.last() else { break };
            cursor = Some((last.due_at, last.id));
            let more = runs.len() == 1000;
            let stale = outbox::Entity::find()
                .secure()
                .scope_with(&scope)
                .filter(
                    Condition::all()
                        .add(outbox::Column::RunId.is_in(runs.iter().map(|r| r.id)))
                        .add(outbox::Column::DeliveredAt.lte(stale_before)),
                )
                .all(&conn)
                .await?
                .into_iter()
                .map(|r| r.run_id)
                .collect::<std::collections::HashSet<_>>();
            for row in runs {
                if !stale.contains(&row.id) {
                    continue;
                }
                let scope = self.worker_scope("dispatch").await?;
                let Some(journal) = self.get(&scope, RunId(row.id)).await? else {
                    continue;
                };
                let publication = std::sync::Arc::new(super::repository::Publication::new(
                    self.publisher.read().clone(),
                ));
                let publisher = publication.clone();
                let scope = scope.clone();
                self.db
                    .db()
                    .transaction_ref_mapped(move |txn| {
                        Box::pin(async move {
                            let changed = outbox::Entity::update_many()
                                .secure()
                                .scope_with(&scope)
                                .filter(
                                    Condition::all()
                                        .add(outbox::Column::RunId.eq(row.id))
                                        .add(
                                            outbox::Column::Generation
                                                .eq(journal.delivery_generation),
                                        )
                                        .add(outbox::Column::DeliveredAt.lte(stale_before)),
                                )
                                .col_expr(
                                    outbox::Column::DeliveredAt,
                                    Expr::value(None::<DateTime<Utc>>),
                                )
                                .exec(txn)
                                .await?;
                            if changed.rows_affected == 1 {
                                publish(txn, &journal, &publisher).await?;
                            }
                            Ok::<(), StoreError>(())
                        })
                    })
                    .await
                    .inspect(|_value| {
                        publication.fire();
                    })?;
            }
            if !more {
                break;
            }
        }
        Ok(())
    }
    pub async fn forward_legacy_deliveries(&self) -> Result<(), StoreError> {
        let scope = self.worker_scope("dispatch").await?;
        let conn = self.db.conn()?;
        let rows = outbox::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(outbox::Column::Forwarded.eq(false)))
            .order_by(outbox::Column::DueAt, sea_orm::Order::Asc)
            .limit(100)
            .all(&conn)
            .await?;
        for row in rows {
            let scope = self.worker_scope("dispatch").await?;
            let publication = std::sync::Arc::new(super::repository::Publication::new(
                self.publisher.read().clone(),
            ));
            let publisher = publication.clone();
            self.db
                .db()
                .transaction_ref_mapped(move |txn| {
                    Box::pin(async move {
                        let changed = outbox::Entity::update_many()
                            .secure()
                            .scope_with(&scope)
                            .filter(
                                Condition::all()
                                    .add(outbox::Column::Id.eq(row.id))
                                    .add(outbox::Column::Forwarded.eq(false)),
                            )
                            .col_expr(outbox::Column::Forwarded, Expr::value(true))
                            .exec(txn)
                            .await?;
                        if changed.rows_affected == 0 {
                            return Ok::<(), StoreError>(());
                        }
                        if let Some(run) = run::Entity::find()
                            .secure()
                            .scope_with(&scope)
                            .filter(Condition::all().add(run::Column::Id.eq(row.run_id)))
                            .one(txn)
                            .await?
                        {
                            let journal = normalized::hydrate(txn, &scope, run).await?;
                            if journal.delivery_generation == row.generation {
                                publish(txn, &journal, &publisher).await?;
                            }
                        }
                        Ok(())
                    })
                })
                .await
                .inspect(|_value| {
                    publication.fire();
                })?;
        }
        Ok(())
    }
}

/// Wakes belong to one transaction. Only its successful commit fires them;
/// dropping a rolled-back or cancelled transaction discards pending signals.
pub(super) struct Publication {
    outbox: Option<Arc<toolkit_db::outbox::Outbox>>,
    pending: parking_lot::Mutex<Vec<toolkit_db::outbox::Wake>>,
}
impl Publication {
    pub(super) fn new(outbox: Option<Arc<toolkit_db::outbox::Outbox>>) -> Self {
        Self {
            outbox,
            pending: parking_lot::Mutex::default(),
        }
    }
    pub(super) fn fire(&self) {
        for wake in self.pending.lock().drain(..) {
            wake.fire();
        }
    }
}
impl Drop for Publication {
    fn drop(&mut self) {
        for wake in self.pending.get_mut().drain(..) {
            wake.discard();
        }
    }
}

pub(super) async fn enqueue<C: DBRunner>(
    runner: &C,
    scope: &AccessScope,
    journal: &Journal,
    publisher: &Publication,
) -> Result<(), StoreError> {
    if matches!(
        journal.run.status,
        RunStatus::Queued | RunStatus::RetryWait | RunStatus::Running
    ) && journal.run.next_attempt_at.is_some()
    {
        let Some(due) = journal.run.next_attempt_at else {
            return Err(StoreError::Invariant("due time missing"));
        };
        let id = Uuid::new_v5(
            &journal.run.id.0,
            &journal.delivery_generation.to_be_bytes(),
        );
        secure_insert::<outbox::Entity>(
            outbox::ActiveModel {
                id: Set(id),
                tenant_id: Set(journal.run.owner.tenant_id),
                owner_id: Set(journal.run.owner.subject_id),
                run_id: Set(journal.run.id.0),
                generation: Set(journal.delivery_generation),
                due_at: Set(due),
                forwarded: Set(true),
                delivered_at: Set(None),
            },
            scope,
            runner,
        )
        .await?;
        publish(runner, journal, publisher).await?;
    }
    Ok(())
}

pub(super) async fn publish<C: DBRunner>(
    runner: &C,
    journal: &Journal,
    publisher: &Publication,
) -> Result<(), StoreError> {
    let Some(activity_id) = journal.delivery_activity() else {
        return Ok(());
    };
    let outbox = publisher
        .outbox
        .clone()
        .ok_or(StoreError::DeliveryUnavailable)?;
    let delivery = crate::infra::apalis::Delivery {
        run_id: journal.run.id.0.to_string(),
        generation: journal.delivery_generation,
        activity_id,
    };
    let record = toolkit_db::outbox::Record::to(
        crate::infra::outbox::QUEUE,
        (journal.run.id.0.as_u128() % 4) as u32,
    )
    .payload(
        serde_json::to_vec(&delivery)?,
        crate::infra::outbox::PAYLOAD_TYPE,
    )
    .build()
    .map_err(StoreError::from)?;
    let wake = outbox
        .enqueue(runner, record)
        .await
        .map_err(StoreError::from)?;
    publisher.pending.lock().push(wake);
    Ok(())
}
pub(super) fn status(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Queued => "queued",
        RunStatus::Running => "running",
        RunStatus::RetryWait => "retry_wait",
        RunStatus::Succeeded => "succeeded",
        RunStatus::Failed => "failed",
        RunStatus::Cancelling => "cancelling",
        RunStatus::Cancelled => "cancelled",
        RunStatus::Blocked => "blocked",
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../../tests/unit/repository_tests.rs"]
pub mod tests;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../../tests/unit/audit_storage_tests.rs"]
mod audit_tests;

impl JournalStore {
    pub async fn list_progress(
        &self,
        scope: &AccessScope,
        query: &durable_execution_sdk::observation::WorkflowQuery,
    ) -> Result<durable_execution_sdk::observation::WorkflowPage, StoreError> {
        self.list_progress_on(&self.db.conn()?, scope, query).await
    }
    pub async fn list_progress_on<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        query: &durable_execution_sdk::observation::WorkflowQuery,
    ) -> Result<durable_execution_sdk::observation::WorkflowPage, StoreError> {
        use durable_execution_sdk::observation::RunStatus as Status;
        use sea_orm::sea_query::{ExprTrait, Query};
        let activity_exists = |status: &str| {
            Expr::exists(
                Query::select()
                    .expr(Expr::val(1))
                    .from(super::activity::Entity)
                    .and_where(
                        Expr::col((super::activity::Entity, super::activity::Column::RunId))
                            .equals((run::Entity, run::Column::Id)),
                    )
                    .and_where(
                        Expr::col((super::activity::Entity, super::activity::Column::Status))
                            .eq(status),
                    )
                    .to_owned(),
            )
        };
        let legacy_failing = if matches!(
            query.status,
            Some(
                Status::Failing
                    | Status::Queued
                    | Status::Running
                    | Status::RetryWait
                    | Status::Failed
            )
        ) {
            self.legacy_failing_ids(db, scope, query.since).await?
        } else {
            Vec::new()
        };
        let failing = Condition::any()
            .add(
                Condition::all()
                    .add(run::Column::Status.is_in(["running", "queued", "retry_wait"]))
                    .add(activity_exists("failed")),
            )
            .add(
                Condition::all()
                    .add(run::Column::Status.eq("failed"))
                    .add(activity_exists("running")),
            )
            .add(run::Column::Id.is_in(legacy_failing));
        let mut filter = Condition::all().add(run::Column::CreatedAt.gte(query.since));
        if let Some(status) = query.status {
            if status == Status::Failing {
                filter = filter.add(failing);
            } else {
                let value = serde_json::to_value(status)?
                    .as_str()
                    .ok_or(StoreError::Invariant("invalid status"))?
                    .to_owned();
                filter = filter.add(run::Column::Status.eq(value)).add(failing.not());
            }
        }
        let total = run::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(filter.clone())
            .count(db)
            .await?;
        let rows = run::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(filter)
            .order_by(run::Column::CreatedAt, sea_orm::Order::Desc)
            .order_by(run::Column::Id, sea_orm::Order::Desc)
            .offset(u64::from(query.offset))
            .limit(u64::from(query.limit.clamp(1, 100)))
            .all(db)
            .await?;
        let mut items = Vec::with_capacity(rows.len());
        for row in rows {
            let journal = normalized::hydrate(db, scope, row).await?;
            items.push(crate::domain::view::progress(&journal)?);
        }
        Ok(durable_execution_sdk::observation::WorkflowPage { items, total })
    }
    async fn legacy_failing_ids<C: DBRunner>(
        &self,
        db: &C,
        scope: &AccessScope,
        since: DateTime<Utc>,
    ) -> Result<Vec<Uuid>, StoreError> {
        let mut after = None;
        let mut ids = Vec::new();
        loop {
            let mut filter = Condition::all()
                .add(run::Column::StorageVersion.eq(0))
                .add(run::Column::CreatedAt.gte(since))
                .add(run::Column::Status.is_in(["running", "queued", "retry_wait", "failed"]));
            if let Some(id) = after {
                filter = filter.add(run::Column::Id.gt(id));
            }
            let rows = run::Entity::find()
                .secure()
                .scope_with(scope)
                .filter(filter)
                .order_by(run::Column::Id, sea_orm::Order::Asc)
                .limit(100)
                .all(db)
                .await?;
            if rows.is_empty() {
                break;
            }
            for row in rows {
                after = Some(row.id);
                let journal = normalized::hydrate(db, scope, row).await?;
                if crate::domain::view::progress(&journal)?.state.status()
                    == durable_execution_sdk::observation::RunStatus::Failing
                {
                    ids.push(journal.run.id.0);
                }
            }
        }
        Ok(ids)
    }
}
