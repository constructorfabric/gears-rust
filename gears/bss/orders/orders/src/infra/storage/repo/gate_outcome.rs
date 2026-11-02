//! The `bss_orders__gate_outcome` row building and insert (S3-10, F03 §3.1), moved out of
//! `infra/gate` so that the gate never reaches the entity or `secure_insert_many` (n172).
//! The precedent is [`super::inflight`]: gate reads live in the repo and take `&impl DBRunner`.
//!
//! [`RunRecord`] and [`RunTarget`] live here because storage must not import from `infra/gate`;
//! `infra::gate::writer` re-exports them.
use crate::domain::gate::MAPPING_VERSION;
use crate::domain::gate::vector::Vector;
use crate::infra::storage::entity::gate_outcome;
use sea_orm::ActiveValue::Set;
use time::OffsetDateTime;
use toolkit_db::secure::{AccessScope, DBRunner, ScopeError, secure_insert_many};
use uuid::Uuid;

/// What a run assessed: a Preview (no order) or a submit/amendment of one order. A submit or
/// amendment run always names its order by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunTarget {
    /// A Preview: rows carry no order and are purged after seven days.
    Preview,
    /// A submit or amendment of `order_id`; `version` is the version the run admitted, if any.
    Order {
        /// The assessed order.
        order_id: Uuid,
        /// The order version the run admitted.
        version: Option<i32>,
    },
}

/// One assessment run to persist: its identity, the caller and scope snapshot it was assessed
/// under, what it assessed (a Preview or one order's version) and the complete outcome vector.
#[derive(Debug, Clone, Copy)]
pub struct RunRecord<'a> {
    /// Run identity shared by every row; returned as `assessmentId`.
    pub run_id: Uuid,
    /// Caller home tenant; the scoping tenant of every row.
    pub subject_tenant_id: Uuid,
    /// Opaque caller principal.
    pub subject_id: &'a str,
    /// Resource tenant of the assessed scope snapshot.
    pub resource_tenant_id: Uuid,
    /// Seller tenant of the assessed scope snapshot.
    pub seller_tenant_id: Uuid,
    /// Payer tenant of the assessed scope snapshot.
    pub payer_tenant_id: Uuid,
    /// Request correlation for diagnostics, if any.
    pub correlation_id: Option<Uuid>,
    /// A Preview, or the assessed order and the version the run admitted.
    pub target: RunTarget,
    /// Run instant, shared by every row: the assessment time, never the engine's transition
    /// time (a submit or amendment run assessed before the transaction keeps its own instant).
    pub evaluated_at: OffsetDateTime,
    /// The complete ordered outcome vector.
    pub vector: &'a Vector,
}

/// The rows of one run, in vector order: a pure 1:1 projection of
/// [`Vector::stored_projection`], each row with a fresh time-ordered `outcome_id`.
///
/// # Errors
/// `ScopeError::Invalid` when the producer results or upstream detail do not serialize.
pub fn rows(run: &RunRecord<'_>) -> Result<Vec<gate_outcome::ActiveModel>, ScopeError> {
    let stored = run
        .vector
        .stored_projection()
        .map_err(|_| ScopeError::Invalid("gate outcome evidence does not serialize"))?;
    let (order_id, version) = match run.target {
        RunTarget::Preview => (None, None),
        RunTarget::Order { order_id, version } => (Some(order_id), version),
    };
    Ok(stored
        .into_iter()
        .map(|o| gate_outcome::ActiveModel {
            outcome_id: Set(Uuid::now_v7()),
            run_id: Set(run.run_id),
            subject_tenant_id: Set(run.subject_tenant_id),
            subject_id: Set(run.subject_id.to_owned()),
            resource_tenant_id: Set(run.resource_tenant_id),
            seller_tenant_id: Set(run.seller_tenant_id),
            payer_tenant_id: Set(run.payer_tenant_id),
            correlation_id: Set(run.correlation_id),
            order_id: Set(order_id),
            version: Set(version),
            line_id: Set(o.line_id),
            predicate: Set(o.predicate.to_owned()),
            item_id: Set(o.item_id),
            has_catalog_selection: Set(o.has_catalog_selection),
            catalog_scope_key: Set(o.catalog_scope_key),
            verdict: Set(o.verdict.to_owned()),
            reason: Set(o.reason.map(str::to_owned)),
            mapping_version: Set(MAPPING_VERSION.to_owned()),
            applicability: Set(o.applicability.to_owned()),
            producer_results: Set(o.producer_results),
            upstream_detail: Set(o.upstream_detail),
            evaluated_at: Set(run.evaluated_at),
        })
        .collect())
}

/// Insert the rows in one statement batch on the caller's transaction; the caller commits.
/// Every row is validated against `scope` before anything is sent.
///
/// `tx` must be a transaction runner, never a bare connection: `secure_insert_many` splits a
/// batch that exceeds the bind-parameter budget into several statements, and a split batch is
/// atomic only inside the caller's transaction. The returned count is the computed row count,
/// not the database's `rows_affected`.
///
/// # Errors
/// A row outside `scope`, or a database refusal.
pub async fn insert(
    tx: &impl DBRunner,
    scope: &AccessScope,
    rows: Vec<gate_outcome::ActiveModel>,
) -> Result<u64, ScopeError> {
    let count = u64::try_from(rows.len())
        .map_err(|_| ScopeError::Invalid("gate outcome batch exceeds u64"))?;
    secure_insert_many::<gate_outcome::Entity>(rows, scope, tx).await?;
    Ok(count)
}
