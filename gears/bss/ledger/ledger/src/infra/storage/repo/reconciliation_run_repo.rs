//! `ReconciliationRunRepo` — the reconciliation-run table
//! (`bss.ledger_reconciliation_run`), keyed by `(tenant_id, run_id)`. The
//! framework `start`s a RUNNING row then `finalize`s it with the variance;
//! an out-of-tolerance run feeds an `exception_queue` row + the close gate
//! (Slice 7, design §4.3).

use sea_orm::sea_query::Expr;
use sea_orm::{ActiveValue::Set, ColumnTrait, Condition, EntityTrait, QuerySelect};
use serde_json::Value as JsonValue;
use toolkit_db::secure::{
    AccessScope, DbTx, SecureDeleteExt, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use toolkit_db::{DBProvider, DbError};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::model::RepoError;
use crate::infra::storage::entity::reconciliation_run;
use time::OffsetDateTime;

/// `status` literal for a finalized run (the value [`ReconciliationRunRepo::finalize`]
/// is called with on the happy path). Named here because the purge predicate
/// below has to match on it.
const STATUS_DONE: &str = "DONE";

/// SeaORM-backed reconciliation-run repository.
#[derive(Clone)]
pub struct ReconciliationRunRepo {
    db: DBProvider<DbError>,
}

impl ReconciliationRunRepo {
    #[must_use]
    pub fn new(db: DBProvider<DbError>) -> Self {
        Self { db }
    }

    /// Create a RUNNING reconciliation-run row.
    ///
    /// # Errors
    /// Returns [`RepoError::Db`] if scope validation or insertion fails.
    pub async fn start(
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        run_id: Uuid,
        period_id: &str,
        check_type: &str,
    ) -> Result<(), RepoError> {
        let am = reconciliation_run::ActiveModel {
            tenant_id: Set(tenant),
            run_id: Set(run_id),
            period_id: Set(period_id.to_owned()),
            check_type: Set(check_type.to_owned()),
            variance_minor: Set(0),
            within_tolerance: Set(true),
            status: Set("RUNNING".to_owned()),
            watermark: Set(None),
            detail: Set(None),
            at_utc: Set(OffsetDateTime::now_utc()),
        };
        reconciliation_run::Entity::insert(am.clone())
            .secure()
            .scope_with_model(scope, &am)
            .map_err(|e| RepoError::Db(format!("ledger_reconciliation_run scope: {e}")))?
            .exec_with_returning(txn)
            .await
            .map_err(|e| RepoError::Db(format!("insert ledger_reconciliation_run: {e}")))?;
        Ok(())
    }

        /// Finalize a run with its variance result.
    ///
    /// # Errors
    /// Returns [`RepoError::Db`] if the scoped update fails.
    #[allow(
        clippy::too_many_arguments,
        reason = "a finalized run records its full variance result in one write"
    )]
    pub async fn finalize(
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        run_id: Uuid,
        status: &str,
        variance_minor: i64,
        within_tolerance: bool,
        watermark: Option<i64>,
        detail: Option<JsonValue>,
    ) -> Result<(), RepoError> {
        reconciliation_run::Entity::update_many()
            .secure()
            .scope_with(scope)
            .col_expr(reconciliation_run::Column::Status, Expr::value(status))
            .col_expr(
                reconciliation_run::Column::VarianceMinor,
                Expr::value(variance_minor),
            )
            .col_expr(
                reconciliation_run::Column::WithinTolerance,
                Expr::value(within_tolerance),
            )
            .col_expr(
                reconciliation_run::Column::Watermark,
                Expr::value(watermark),
            )
            .col_expr(reconciliation_run::Column::Detail, Expr::value(detail))
            .filter(
                Condition::all()
                    .add(reconciliation_run::Column::TenantId.eq(tenant))
                    .add(reconciliation_run::Column::RunId.eq(run_id)),
            )
            .exec(txn)
            .await
            .map_err(|e| RepoError::Db(format!("finalize ledger_reconciliation_run: {e}")))?;
        Ok(())
    }

    /// Read a run (out-of-txn). SQL-level BOLA: a foreign tenant yields no row.
    ///
    /// # Errors
    /// Returns [`DomainError::Internal`] if acquiring a connection or reading
    /// the run fails.
    pub async fn read(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        run_id: Uuid,
    ) -> Result<Option<reconciliation_run::Model>, DomainError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| DomainError::Internal(format!("conn: {e}")))?;
        let row = reconciliation_run::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(
                Condition::all()
                    .add(reconciliation_run::Column::TenantId.eq(tenant))
                    .add(reconciliation_run::Column::RunId.eq(run_id)),
            )
            .one(&conn)
            .await
            .map_err(|e| DomainError::Internal(format!("read ledger_reconciliation_run: {e}")))?;
        Ok(row)
    }

    /// Delete up to `limit` **uneventful** runs of ONE tenant — finalized
    /// (`DONE`) runs that recorded no variance (`within_tolerance = true`).
    /// Returns the number of rows actually deleted, so the caller can tell a
    /// drained tenant (`< limit`) from one with more to give.
    ///
    /// Used by the reconciliation tick to reclaim the runs it accumulated for
    /// tenants that have since left the platform registry (soft-deleted or
    /// hard-deleted). Two properties make this safe to run on a multi-GB table:
    ///
    /// * **Evidence is never deleted.** A run that breached tolerance, or that
    ///   never finalized (`RUNNING` / `FAILED`), is kept whatever the tenant's
    ///   lifecycle — those are the rows that record that something was once
    ///   wrong. Only the "checked it, nothing to see" rows go.
    /// * **No sequential scan.** The delete is scoped to a single tenant, so it
    ///   rides the `(tenant_id, run_id)` primary key. (The table has no index
    ///   on `at_utc`, which is exactly why an age-based purge would have to
    ///   scan the whole 23 GB heap instead.)
    ///
    /// # Errors
    /// Returns [`RepoError::Db`] if acquiring a connection, selecting the batch,
    /// or the scoped delete fails.
    pub async fn purge_uneventful_runs(&self, tenant: Uuid, limit: u64) -> Result<u64, RepoError> {
        #[derive(Debug, sea_orm::FromQueryResult)]
        struct RunIdRow {
            run_id: Uuid,
        }

        if limit == 0 {
            return Ok(0);
        }
        let scope = AccessScope::for_tenant(tenant);
        let conn = self
            .db
            .conn()
            .map_err(|e| RepoError::Db(format!("conn: {e}")))?;
        let purgeable = Condition::all()
            .add(reconciliation_run::Column::Status.eq(STATUS_DONE))
            .add(reconciliation_run::Column::WithinTolerance.eq(true));

        // Pick the batch first, then delete it by key. Postgres has no
        // `DELETE … LIMIT`, and an unbounded per-tenant delete would be
        // unbounded WAL for a tenant that happens to hold millions of rows.
        let batch = reconciliation_run::Entity::find()
            .secure()
            .scope_with(&scope)
            .filter(purgeable)
            .project_all(&conn, |q| {
                q.select_only()
                    .column(reconciliation_run::Column::RunId)
                    .limit(limit)
                    .into_model::<RunIdRow>()
            })
            .await
            .map_err(|e| RepoError::Db(format!("select purgeable reconciliation runs: {e}")))?;
        if batch.is_empty() {
            return Ok(0);
        }

        let run_ids: Vec<Uuid> = batch.into_iter().map(|r| r.run_id).collect();
        let deleted = reconciliation_run::Entity::delete_many()
            .secure()
            .scope_with(&scope)
            .filter(Condition::all().add(reconciliation_run::Column::RunId.is_in(run_ids)))
            .exec(&conn)
            .await
            .map_err(|e| RepoError::Db(format!("purge ledger_reconciliation_run: {e}")))?;
        Ok(deleted.rows_affected)
    }
}
