//! Scoped typed reconciliation diagnostics and conservative bounded deletion.
use crate::domain::model::RepoError;
use crate::domain::reconciliation::ReconciliationVariance;
use crate::domain::status::{RECON_RUN_STATUS_DONE, RECON_RUN_STATUS_RUNNING};
use crate::infra::posting::retry::{db_to_repo, scope_to_repo};
use crate::infra::storage::entity::reconciliation_run::{self, Column as C};
use sea_orm::sea_query::{Alias, Expr};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, Condition, EntityTrait, ExprTrait, QueryOrder, QuerySelect,
};
use serde_json::Value as JsonValue;
use time::OffsetDateTime;
use toolkit_db::secure::{
    AccessScope, DBRunner, DbTx, SecureDeleteExt, SecureEntityExt, SecureInsertExt, SecureUpdateExt,
};
use toolkit_db::{DBProvider, DbError};
use uuid::Uuid;
#[path = "reconciliation_variance.rs"]
mod variance;

/// A complete validated diagnostic record.
#[derive(Clone, Debug)]
pub struct ReconciliationRunView {
    pub tenant_id: Uuid,
    pub run_id: Uuid,
    pub period_id: String,
    pub check_type: String,
    pub variance: ReconciliationVariance,
    pub within_tolerance: bool,
    pub status: String,
    pub watermark: Option<i64>,
    pub detail: Option<JsonValue>,
    pub at_utc: OffsetDateTime,
}

/// Raw JSON text prevents malformed SQLite JSON from aborting a purge scan.
#[derive(Debug, sea_orm::FromQueryResult)]
struct StoredRun {
    tenant_id: Uuid,
    run_id: Uuid,
    period_id: String,
    check_type: String,
    variance: String,
    within_tolerance: bool,
    status: String,
    watermark: Option<i64>,
    detail: Option<String>,
    at_utc: OffsetDateTime,
}

/// The caller owns transaction retries; no diagnostic version schema is needed.
#[derive(Clone)]
pub struct ReconciliationRunRepo {
    db: DBProvider<DbError>,
}

impl ReconciliationRunRepo {
    /// Build the repo over one database provider.
    #[must_use]
    pub fn new(db: DBProvider<DbError>) -> Self {
        Self { db }
    }

    /// Insert RUNNING with the check's typed zero. Duplicate run identity is an error.
    ///
    /// # Errors
    /// The variance codec's [`RepoError`] when `check_type` is unknown or its typed zero cannot
    /// be encoded; [`RepoError::Db`] on a scope or storage failure (a duplicate run identity
    /// included); [`RepoError::Conflict`] on classified database contention.
    pub async fn start(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        run_id: Uuid,
        period_id: &str,
        check_type: &str,
    ) -> Result<(), RepoError> {
        let zero = variance::zero(check_type)?;
        let am = reconciliation_run::ActiveModel {
            tenant_id: Set(tenant),
            run_id: Set(run_id),
            period_id: Set(period_id.to_owned()),
            check_type: Set(check_type.to_owned()),
            variance: Set(variance::encode(check_type, &zero)?),
            within_tolerance: Set(true),
            status: Set(RECON_RUN_STATUS_RUNNING.to_owned()),
            watermark: Set(None),
            detail: Set(None),
            at_utc: Set(OffsetDateTime::now_utc()),
        };
        reconciliation_run::Entity::insert(am.clone())
            .secure()
            .scope_with_model(scope, &am)
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?
            .exec(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        Ok(())
    }

    /// Replace diagnostics after validating both existing and new evidence.
    /// A stale observation conflicts and must abort the caller's whole attempt.
    ///
    /// # Errors
    /// [`RepoError::InvalidRequest`] when `status` is not a valid run status or the run does
    /// not exist; the variance codec's [`RepoError`] when the stored or new variance is
    /// malformed; [`RepoError::InvalidStoredMoney`] when the stored run fails validation;
    /// [`RepoError::Conflict`] when the observed row changed underneath, or on classified
    /// database contention; [`RepoError::Db`] on a scope or storage failure.
    #[allow(
        clippy::too_many_arguments,
        reason = "one atomic diagnostic replacement"
    )]
    pub async fn finalize(
        &self,
        txn: &DbTx<'_>,
        scope: &AccessScope,
        tenant: Uuid,
        run_id: Uuid,
        status: &str,
        result: &ReconciliationVariance,
        within_tolerance: bool,
        watermark: Option<i64>,
        detail: Option<JsonValue>,
    ) -> Result<(), RepoError> {
        validate_status(status).map_err(RepoError::InvalidRequest)?;
        let old = self
            .stored(txn, scope, key(tenant, run_id), 1)
            .await?
            .pop()
            .ok_or_else(|| RepoError::InvalidRequest("reconciliation run not found".into()))?;
        decode(&old)?;
        let encoded = variance::encode(&old.check_type, result)?;
        let changed = reconciliation_run::Entity::update_many()
            .secure()
            .scope_with(scope)
            .col_expr(C::Status, Expr::value(status))
            .col_expr(C::Variance, Expr::value(encoded))
            .col_expr(C::WithinTolerance, Expr::value(within_tolerance))
            .col_expr(C::Watermark, Expr::value(watermark))
            .col_expr(C::Detail, Expr::value(detail))
            .filter(observed(&old))
            .exec(txn)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        if changed.rows_affected != 1 {
            return Err(RepoError::Conflict("reconciliation run changed".into()));
        }
        Ok(())
    }

    /// Standalone typed read, sharing the strict caller-runner decoder.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired or on a scope / storage failure;
    /// [`RepoError::Conflict`] on classified database contention;
    /// [`RepoError::InvalidStoredMoney`] (or the variance codec's [`RepoError`]) when the
    /// stored run is malformed.
    pub async fn read(
        &self,
        scope: &AccessScope,
        tenant: Uuid,
        run_id: Uuid,
    ) -> Result<Option<ReconciliationRunView>, RepoError> {
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        self.read_in(&conn, scope, tenant, run_id).await
    }

    /// Read inside a caller's transaction without a registry or implicit repair.
    ///
    /// # Errors
    /// [`RepoError::Db`] on a scope or storage failure; [`RepoError::Conflict`] on classified
    /// database contention; [`RepoError::InvalidStoredMoney`] (or the variance codec's
    /// [`RepoError`]) when the stored run is malformed.
    pub async fn read_in<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        tenant: Uuid,
        run_id: Uuid,
    ) -> Result<Option<ReconciliationRunView>, RepoError> {
        self.stored(runner, scope, key(tenant, run_id), 1)
            .await?
            .pop()
            .as_ref()
            .map(decode)
            .transpose()
    }

    /// Delete at most `limit` validated zero DONE runs within tolerance.
    /// Traverse stable scoped key batches past preserved evidence. Memory and deletion
    /// are bounded; scanning work can cover the tenant's entire preserved history.
    /// Structural failures are retained by their existing `within_tolerance=false` contract.
    /// A stored run that does not decode is retained, skipped and logged at `warn`; it never
    /// fails the purge.
    ///
    /// # Errors
    /// [`RepoError::Db`] when no connection can be acquired, or when selecting a batch or the
    /// scoped delete fails; [`RepoError::Conflict`] on classified database contention.
    pub async fn purge_uneventful_runs(&self, tenant: Uuid, limit: u64) -> Result<u64, RepoError> {
        if limit == 0 {
            return Ok(0);
        }
        let scope = AccessScope::for_tenant(tenant);
        let conn = self
            .db
            .conn()
            .map_err(|e| db_to_repo(e, self.db.db().backend()))?;
        let mut cursor = None;
        let mut deleted = 0;
        loop {
            let mut condition = eligible().add(C::TenantId.eq(tenant));
            if let Some(id) = cursor {
                condition = condition.add(C::RunId.gt(id));
            }
            let batch = self.stored(&conn, &scope, condition, 128).await?;
            let Some(last) = batch.last() else {
                break;
            };
            cursor = Some(last.run_id);
            let room = usize::try_from(limit - deleted).unwrap_or(usize::MAX);
            let zero: Vec<&StoredRun> = batch
                .iter()
                .filter(|row| observed_zero(row))
                .take(room)
                .collect();
            deleted += self.delete_observed(&conn, &scope, &zero).await?;
            if deleted >= limit {
                return Ok(deleted);
            }
        }
        Ok(deleted)
    }

    /// Single-row form of the page delete: recheck the typed zero, then delete
    /// under the row's observed evidence. The evidence-guard tests drive it.
    #[cfg(test)]
    async fn delete_observed_zero<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        row: &StoredRun,
    ) -> Result<u64, RepoError> {
        if !observed_zero(row) {
            return Ok(0);
        }
        self.delete_observed(runner, scope, &[row]).await
    }

    /// One delete for a page of validated zero runs, each guarded by all of its
    /// observed evidence, so a run changed since the read is kept.
    async fn delete_observed<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        rows: &[&StoredRun],
    ) -> Result<u64, RepoError> {
        if rows.is_empty() {
            return Ok(0);
        }
        let any_observed = rows
            .iter()
            .fold(Condition::any(), |any, row| any.add(observed(row)));
        let deleted = reconciliation_run::Entity::delete_many()
            .secure()
            .scope_with(scope)
            .filter(eligible().add(any_observed))
            .exec(runner)
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))?;
        Ok(deleted.rows_affected)
    }

    /// JSON-to-text is portable and only transports evidence; it performs no money arithmetic.
    async fn stored<R: DBRunner>(
        &self,
        runner: &R,
        scope: &AccessScope,
        condition: Condition,
        limit: u64,
    ) -> Result<Vec<StoredRun>, RepoError> {
        reconciliation_run::Entity::find()
            .secure()
            .scope_with(scope)
            .filter(condition)
            .project_all(runner, |q| {
                q.select_only()
                    .columns([
                        C::TenantId,
                        C::RunId,
                        C::PeriodId,
                        C::CheckType,
                        C::WithinTolerance,
                        C::Status,
                        C::Watermark,
                        C::AtUtc,
                    ])
                    .expr_as(
                        Expr::col(C::Variance).cast_as(Alias::new("text")),
                        "variance",
                    )
                    .expr_as(Expr::col(C::Detail).cast_as(Alias::new("text")), "detail")
                    .order_by_asc(C::RunId)
                    .limit(limit)
                    .into_model::<StoredRun>()
            })
            .await
            .map_err(|e| scope_to_repo(e, self.db.db().backend()))
    }
}

/// Recheck exact typed zero in Rust. A run that does not decode is evidence:
/// it is kept and logged, never deleted and never a purge failure.
fn observed_zero(row: &StoredRun) -> bool {
    match decode(row) {
        Ok(view) => {
            view.status == RECON_RUN_STATUS_DONE && view.within_tolerance && view.variance.is_zero()
        }
        Err(error) => {
            tracing::warn!(
                tenant_id = %row.tenant_id,
                run_id = %row.run_id,
                check_type = %row.check_type,
                error = %error,
                "reconciliation purge: stored run does not decode; retained"
            );
            false
        }
    }
}
/// Only nonmoney selection predicates. Typed zero is checked in Rust.
fn eligible() -> Condition {
    Condition::all()
        .add(C::Status.eq(RECON_RUN_STATUS_DONE))
        .add(C::WithinTolerance.eq(true))
}
/// Tenant and diagnostic identity.
fn key(tenant: Uuid, run_id: Uuid) -> Condition {
    Condition::all()
        .add(C::TenantId.eq(tenant))
        .add(C::RunId.eq(run_id))
}
/// Guard even changed detail/metadata, with null-safe optional comparisons.
fn observed(row: &StoredRun) -> Condition {
    let condition = key(row.tenant_id, row.run_id)
        .add(C::PeriodId.eq(&row.period_id))
        .add(C::CheckType.eq(&row.check_type))
        .add(C::Status.eq(&row.status))
        .add(C::WithinTolerance.eq(row.within_tolerance))
        .add(C::AtUtc.eq(row.at_utc))
        .add(
            Expr::col(C::Variance)
                .cast_as(Alias::new("text"))
                .eq(row.variance.clone()),
        );
    let condition = match row.watermark {
        Some(v) => condition.add(C::Watermark.eq(v)),
        None => condition.add(C::Watermark.is_null()),
    };
    match &row.detail {
        Some(v) => condition.add(
            Expr::col(C::Detail)
                .cast_as(Alias::new("text"))
                .eq(v.clone()),
        ),
        None => condition.add(C::Detail.is_null()),
    }
}
/// Keep the existing diagnostic status set.
fn validate_status(status: &str) -> Result<(), String> {
    match status {
        RECON_RUN_STATUS_RUNNING | RECON_RUN_STATUS_DONE | "FAILED" => Ok(()),
        _ => Err("unknown reconciliation status".into()),
    }
}
/// Read validation applies to the complete stored record before use or replacement.
fn decode(row: &StoredRun) -> Result<ReconciliationRunView, RepoError> {
    validate_status(&row.status).map_err(RepoError::InvalidStoredMoney)?;
    let variance = variance::decode(&row.check_type, &row.variance)?;
    let detail = row
        .detail
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|e| RepoError::InvalidStoredMoney(format!("reconciliation detail: {e}")))?;
    Ok(ReconciliationRunView {
        tenant_id: row.tenant_id,
        run_id: row.run_id,
        period_id: row.period_id.clone(),
        check_type: row.check_type.clone(),
        variance,
        within_tolerance: row.within_tolerance,
        status: row.status.clone(),
        watermark: row.watermark,
        detail,
        at_utc: row.at_utc,
    })
}

#[cfg(test)]
#[path = "reconciliation_run_repo_tests.rs"]
mod tests;
