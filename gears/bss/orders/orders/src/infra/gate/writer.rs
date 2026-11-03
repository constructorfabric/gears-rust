//! The atomic gate diagnostics writer over `bss_orders__gate_outcome` (S3-10, F03 §3.1,
//! DESIGN `#contract-03-3-7`).
//!
//! One assessment run is one batch: every outcome of the vector is inserted on one transaction
//! or none is. A submit or amendment run is written on the engine's transaction beside the
//! document it admits; a Preview run owns a short transaction and reports its `run_id` only
//! after the commit. Preview rows (`order_id` NULL) are purged after seven days by
//! `repo::retention::preview_diagnostics`.
use crate::domain::gate::vector::Vector;
use crate::infra::engine::documents::{DiagnosticsContribution, DiagnosticsSettled};
use crate::infra::storage::repo::gate_outcome;
pub use crate::infra::storage::repo::gate_outcome::{RunRecord, RunTarget};
use async_trait::async_trait;
use time::OffsetDateTime;
use toolkit_db::secure::{AccessScope, DBRunner, ScopeError};
use toolkit_db::{Db, DbError, DbTx};
use uuid::Uuid;

/// Insert every outcome of the run in one statement batch on the caller's transaction; the
/// caller commits (Preview: its own short transaction; submit: the engine's, as a document,
/// with a [`RunTarget::Order`] target).
/// Every row is validated against `scope` before anything is sent, so a scope violation
/// anywhere inserts nothing; a database refusal fails the caller's transaction as a whole.
///
/// `tx` must be a transaction runner, never a bare connection: the repo insert splits a
/// batch that exceeds the bind-parameter budget into several statements, and a split batch is
/// atomic only inside the caller's transaction. The returned count is the computed row count of
/// the run, not the database's `rows_affected`.
///
/// # Errors
/// Unserializable evidence, a row outside `scope`, or a database refusal.
pub async fn write_run(
    tx: &impl DBRunner,
    scope: &AccessScope,
    run: &RunRecord<'_>,
) -> Result<u64, ScopeError> {
    gate_outcome::insert(tx, scope, gate_outcome::rows(run)?).await
}

/// A submit or amendment run as the engine's diagnostics contribution (M2-2): the record inputs
/// the slice assessed, written by the engine inside the transition transaction with a
/// [`RunTarget::Order`] target and the caller's subject identity taken from what the engine
/// settled. It carries no subject identity of its own.
#[derive(Debug, Clone)]
pub struct GateRun {
    /// Resolved overlap key of every line in the frozen run.
    pub overlap_keys: std::collections::BTreeMap<Uuid, std::collections::BTreeSet<String>>,
    /// Run identity shared by every row; returned as `assessmentId`.
    pub run_id: Uuid,
    /// Resource tenant of the assessed scope snapshot.
    pub resource_tenant_id: Uuid,
    /// Seller tenant of the assessed scope snapshot.
    pub seller_tenant_id: Uuid,
    /// Payer tenant of the assessed scope snapshot.
    pub payer_tenant_id: Uuid,
    /// Request correlation for diagnostics, if any.
    pub correlation_id: Option<Uuid>,
    /// The assessment time (`RunRecord::evaluated_at`), not the transition time.
    pub evaluated_at: OffsetDateTime,
    /// The complete ordered outcome vector.
    pub vector: Vector,
}
impl GateRun {
    fn collision_vector(
        &self,
        conflict: &crate::domain::overlap::OverlapConflict,
    ) -> Result<Vector, ScopeError> {
        let replacement = conflict.predicate_nine();
        let lines = self
            .overlap_keys
            .iter()
            .filter_map(|(line, key)| {
                replacement
                    .blocked
                    .iter()
                    .any(|b| {
                        b.tuple.payer_tenant_id == self.payer_tenant_id
                            && b.tuple.resource_tenant_id == self.resource_tenant_id
                            && key.contains(b.tuple.overlap_scope_key.as_str())
                    })
                    .then_some(*line)
            })
            .collect();
        self.vector.replace_inflight(&lines).map_err(|error| {
            tracing::error!(
                target: "orders.gate.writer",
                run_id = %self.run_id,
                error = %error,
                "invalid predicate-9 replacement"
            );
            ScopeError::Invalid("invalid predicate-9 replacement")
        })
    }

    /// The record of this run against the order, version and caller subject the engine
    /// settled.
    #[must_use]
    pub fn record<'a>(&'a self, settled: &'a DiagnosticsSettled) -> RunRecord<'a> {
        RunRecord {
            run_id: self.run_id,
            subject_tenant_id: settled.subject_tenant_id,
            subject_id: &settled.subject_id,
            resource_tenant_id: self.resource_tenant_id,
            seller_tenant_id: self.seller_tenant_id,
            payer_tenant_id: self.payer_tenant_id,
            correlation_id: self.correlation_id,
            target: RunTarget::Order {
                order_id: settled.order_id,
                version: settled.version,
            },
            evaluated_at: self.evaluated_at,
            vector: &self.vector,
        }
    }
}
#[async_trait]
impl DiagnosticsContribution for GateRun {
    fn collision_report(
        &self,
        conflict: &crate::domain::overlap::OverlapConflict,
    ) -> Result<Option<serde_json::Value>, ScopeError> {
        serde_json::to_value(self.collision_vector(conflict)?.report(self.run_id))
            .map(Some)
            .map_err(|_| ScopeError::Invalid("collision report does not serialize"))
    }

    /// The rows are written under the engine's subject-tenant `scope` (the caller's home
    /// tenant), as a Preview run is; the order, version and subject identity come only from
    /// the engine's `settled`.
    async fn write(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        settled: &DiagnosticsSettled,
    ) -> Result<u64, ScopeError> {
        let replacement = settled
            .overlap_conflict
            .as_ref()
            .map(|c| self.collision_vector(c))
            .transpose()?;
        let mut record = self.record(settled);
        if let Some(vector) = &replacement {
            record.vector = vector;
        }
        write_run(tx, scope, &record).await
    }
}

/// Preview: a standalone transaction that commits the vector and returns `run_id` only on commit.
///
/// # Errors
/// A run whose target is not [`RunTarget::Preview`], unserializable evidence, a row outside `scope`, a database refusal
/// or a failed commit; nothing is persisted in any of these cases.
pub async fn write_preview_run(
    db: &Db,
    scope: &AccessScope,
    run: &RunRecord<'_>,
) -> Result<Uuid, ScopeError> {
    if run.target != RunTarget::Preview {
        return Err(ScopeError::Invalid("a Preview run carries no order"));
    }
    // Built before the transaction: the closure must own what it writes.
    let rows = gate_outcome::rows(run)?;
    let scope = scope.clone();
    db.transaction_ref_mapped(move |tx| {
        Box::pin(async move {
            gate_outcome::insert(tx, &scope, rows)
                .await
                .map(|_| ())
                .map_err(PreviewTxError::Scope)
        })
    })
    .await
    .map_err(PreviewTxError::into_scope)?;
    Ok(run.run_id)
}

/// The Preview transaction's failure: the write's own, or begin/commit.
enum PreviewTxError {
    Scope(ScopeError),
    Tx(DbError),
}
impl From<DbError> for PreviewTxError {
    fn from(error: DbError) -> Self {
        Self::Tx(error)
    }
}
impl PreviewTxError {
    fn into_scope(self) -> ScopeError {
        match self {
            Self::Scope(error) => error,
            Self::Tx(DbError::Sea(error)) => ScopeError::Db(error),
            Self::Tx(error) => ScopeError::Db(sea_orm::DbErr::Custom(error.to_string())),
        }
    }
}
