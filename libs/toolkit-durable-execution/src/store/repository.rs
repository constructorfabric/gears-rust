use super::{normalized, outbox, run};
use crate::RunId;
use crate::authorization::ServiceAction;
use crate::error::domain::DomainError;
use crate::state::journal::Journal;
use crate::state::persisted::RunStatus;
use chrono::{DateTime, Utc};
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

/// Failures of journal persistence, delivery publication and worker authorization.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// A secure scoped query failed, including unique-key violations.
    #[error("journal scope operation failed")]
    Scope(#[from] toolkit_db::secure::ScopeError),
    /// The database connection or statement failed.
    #[error("journal database operation failed")]
    Database(#[from] DbError),
    /// A persisted row could not be serialized or deserialized.
    #[error("journal serialization failed")]
    Codec(#[from] serde_json::Error),
    /// Enqueueing a delivery record into the outbox failed.
    #[error("outbox publication failed")]
    Outbox(#[from] toolkit_db::outbox::OutboxError),
    /// Delivery is configured, but its outbox publisher is not installed yet.
    #[error("delivery pipeline is unavailable")]
    DeliveryUnavailable,
    /// This store has no delivery: scheduled work can never be published.
    #[error("delivery is disabled")]
    DeliveryDisabled,
    /// Service authorization rejected or could not evaluate the action.
    #[error("worker authorization failed")]
    Authorization(#[source] crate::authorization::AuthorizationError),
    /// No worker authorization is configured on the store.
    #[error("worker authorization is unavailable")]
    AuthorizationUnavailable,
    /// A compare-and-swap lost a race or observed a torn read; retryable.
    #[error("journal conflict")]
    Conflict,
    /// Persisted state violates an invariant (overflow, corrupt rows).
    #[error("journal invariant violated: {0}")]
    Invariant(&'static str),
    /// The caller lacks the unconstrained scope that definition management requires.
    #[error("definition management access denied")]
    Forbidden,
    /// The run's definition is missing or no longer admits this registration generation.
    #[error("definition is inactive")]
    DefinitionInactive,
    /// The named idempotency or coalescing key was reused with different input.
    #[error("{0} input conflict")]
    RequestConflict(&'static str),
    /// A domain rule rejected the operation.
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
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::Domain(e) => e,
            other => Self::from_store(&other),
        }
    }
}
impl DomainError {
    /// The single store-to-domain classification; logs failures whose details the
    /// domain error cannot carry.
    pub(crate) fn from_store(e: &StoreError) -> Self {
        match e {
            StoreError::Domain(e) => e.clone(),
            StoreError::Forbidden => Self::Forbidden,
            StoreError::Authorization(e)
                if e.kind() == crate::authorization::AuthorizationErrorKind::Denied =>
            {
                Self::Forbidden
            }
            StoreError::DefinitionInactive => Self::DefinitionInactive,
            StoreError::DeliveryDisabled => Self::DeliveryDisabled,
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
    authorization: Option<Arc<dyn crate::authorization::worker::WorkerAuthorization>>,
    pub(super) db: Arc<DBProvider<DbError>>,
    pub(super) publisher: Arc<parking_lot::RwLock<Option<Arc<toolkit_db::outbox::Outbox>>>>,
    /// Whether an outbox publisher is expected; see [`Self::with_delivery`].
    delivers: bool,
}
#[derive(sea_orm::FromQueryResult)]
struct DueRun {
    id: Uuid,
    due_at: DateTime<Utc>,
}
impl JournalStore {
    /// Creates a store without worker authorization or an outbox publisher.
    #[must_use]
    pub fn new(db: Arc<DBProvider<DbError>>) -> Self {
        Self {
            #[cfg(all(test, feature = "integration"))]
            test_database: None,
            authorization: None,
            db,
            publisher: Arc::default(),
            delivers: false,
        }
    }
    /// Declares that this store publishes work once its outbox starts. Until then
    /// scheduling fails as unavailable instead of disabled.
    pub(crate) const fn with_delivery(mut self) -> Self {
        self.delivers = true;
        self
    }
    /// Collects the deliveries of one transaction for the installed publisher.
    pub(super) fn publication(&self) -> Arc<Publication> {
        Arc::new(Publication::new(
            self.publisher.read().clone(),
            self.delivers,
        ))
    }
    /// Sets the worker authorization used to derive background scopes.
    pub(crate) fn with_authorization(
        mut self,
        authorization: Arc<dyn crate::authorization::worker::WorkerAuthorization>,
    ) -> Self {
        self.authorization = Some(authorization);
        self
    }
    /// Resolves the worker access scope for `action`; fails when no authorization is configured.
    pub(crate) async fn worker_scope(
        &self,
        action: crate::authorization::ServiceAction,
    ) -> Result<AccessScope, StoreError> {
        if let Some(authorization) = &self.authorization {
            return authorization.scope(action).await;
        }
        Err(StoreError::AuthorizationUnavailable)
    }
    /// Starts the delivery outbox with `handler` and installs it as the run publisher.
    pub async fn start_outbox(
        &self,
        handler: impl toolkit_db::outbox::LeasedMessageHandler + 'static,
    ) -> anyhow::Result<toolkit_db::outbox::OutboxHandle> {
        let handle = crate::delivery::outbox::start(self.db.db().clone(), handler).await?;
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
        let publication = self.publication();
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
                            fingerprint: Set(journal.fingerprint.clone()),
                            revision: Set(0),
                            registration_generation: Set(i64::try_from(
                                journal.registration_generation,
                            )
                            .map_err(|_| {
                                StoreError::Invariant("registration generation out of range")
                            })?),
                            status: Set(status(journal.run.status).into()),
                            journal: Set(normalized::metadata(&journal)?),
                            progress: Set(Vec::new()),
                            progress_status: Set("queued".into()),
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
    /// Loads a run journal visible to `scope`; `None` when absent.
    pub async fn get(&self, scope: &AccessScope, id: RunId) -> Result<Option<Journal>, StoreError> {
        self.get_on(&self.db.conn()?, scope, id).await
    }
    /// Loads and hydrates a run journal on `conn`, retrying torn reads; `None` when absent.
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
        let publication = self.publication();
        let publisher = publication.clone();
        let catalog_scope = self.worker_scope(ServiceAction::DefinitionGet).await?;
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
    /// Acknowledges a delivery intent on `conn`; a visible prior acknowledgement also succeeds.
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
    /// Returns up to 100 runs with expired claim leases after the `(lease_until, id)` cursor,
    /// plus the next cursor when the page is full.
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
            .add(run::Column::Status.is_in(["running", "cancelling", "compensating"]))
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
    /// Re-opens stale unclaimed deliveries, limited to one definition when `definition` is set.
    pub(crate) async fn recover_unclaimed_deliveries_for_definition(
        &self,
        now: DateTime<Utc>,
        stale_before: DateTime<Utc>,
        definition: Option<&str>,
    ) -> Result<(), StoreError> {
        let scope = self.worker_scope(ServiceAction::Dispatch).await?;
        let conn = self.db.conn()?;
        let mut cursor = None;
        loop {
            let mut filter = Condition::all()
                .add(run::Column::Status.is_in(["queued", "retry_wait", "running", "compensating"]))
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
                let scope = self.worker_scope(ServiceAction::Dispatch).await?;
                let Some(journal) = self.get(&scope, RunId(row.id)).await? else {
                    continue;
                };
                let publication = self.publication();
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
}

/// Wakes belong to one transaction. Only its successful commit fires them;
/// dropping a rolled-back or cancelled transaction discards pending signals.
pub(super) struct Publication {
    outbox: Option<Arc<toolkit_db::outbox::Outbox>>,
    /// Delivery is expected even while `outbox` is not installed yet.
    delivers: bool,
    pending: parking_lot::Mutex<Vec<toolkit_db::outbox::Wake>>,
}
impl Publication {
    pub(super) fn new(outbox: Option<Arc<toolkit_db::outbox::Outbox>>, delivers: bool) -> Self {
        Self {
            outbox,
            delivers,
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
        RunStatus::Queued | RunStatus::RetryWait | RunStatus::Running | RunStatus::Compensating
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
    let activities = journal.publication_activities();
    if activities.is_empty() {
        return Ok(());
    }
    let outbox = publisher.outbox.clone().ok_or(if publisher.delivers {
        StoreError::DeliveryUnavailable
    } else {
        StoreError::DeliveryDisabled
    })?;
    for activity_id in activities {
        let delivery = crate::delivery::apalis::Delivery {
            instance: journal
                .execution
                .instances
                .iter()
                .find(|i| i.state.id.0 == activity_id)
                .map_or_else(
                    || crate::ActivityInstanceId {
                        step: crate::ActivityId(activity_id.clone()),
                        item_index: None,
                        phase: crate::ExecutionPhase::Forward,
                    },
                    |i| i.identity.clone(),
                ),
            run_id: journal.run.id.0.to_string(),
            generation: journal.delivery_generation,
            activity_id,
        };
        let record = toolkit_db::outbox::Record::to(
            crate::delivery::outbox::QUEUE,
            (journal.run.id.0.as_u128() % 4) as u32,
        )
        .payload(
            serde_json::to_vec(&delivery)?,
            crate::delivery::outbox::PAYLOAD_TYPE,
        )
        .build()
        .map_err(StoreError::from)?;
        let wake = outbox
            .enqueue(runner, record)
            .await
            .map_err(StoreError::from)?;
        publisher.pending.lock().push(wake);
    }
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
        RunStatus::Compensating => "compensating",
        RunStatus::CompensationFailed => "compensation_failed",
        RunStatus::Compensated => "compensated",
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/repository_tests.rs"]
pub mod tests;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/audit_storage_tests.rs"]
mod audit_tests;
