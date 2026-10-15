//! Bounded deletion entry points. Database role/trigger checks independently enforce the windows.
//!
//! Each entry accepts only a [`TargetScope`] built from discovered retention candidates (D-184);
//! IDs and cutoff come from that discovery, never from a caller.
use super::{EntityTrait, ScopeError, TransactionRunner, Uuid};
use crate::infra::maintenance::{RetentionTable, TargetScope};
use crate::infra::storage::entity;
use sea_orm::{ColumnTrait, QueryFilter};
use toolkit_db::secure::SecureDeleteExt;
fn bounded(ids: &[Uuid]) -> Result<(), ScopeError> {
    if ids.len() > 5000 {
        return Err(ScopeError::Invalid("retention batch exceeds 5000"));
    }
    Ok(())
}
pub async fn refused_audit(
    tx: &impl TransactionRunner,
    target: &TargetScope,
) -> Result<u64, ScopeError> {
    let (ids, cutoff) = target
        .retention(RetentionTable::RefusedAudit)
        .ok_or(ScopeError::Invalid("not a refused_audit retention target"))?;
    let scope = target.access_scope();
    bounded(ids)?;
    if ids.is_empty() {
        return Ok(0);
    }
    Ok(entity::transition_audit::Entity::delete_many()
        .filter(entity::transition_audit::Column::AuditId.is_in(ids.iter().copied()))
        .filter(entity::transition_audit::Column::Outcome.eq("refused"))
        .filter(entity::transition_audit::Column::CreatedAt.lt(cutoff))
        .secure()
        .scope_with(scope)
        .exec(tx)
        .await?
        .rows_affected)
}
pub async fn preview_diagnostics(
    tx: &impl TransactionRunner,
    target: &TargetScope,
) -> Result<u64, ScopeError> {
    let (ids, cutoff) =
        target
            .retention(RetentionTable::PreviewDiagnostics)
            .ok_or(ScopeError::Invalid(
                "not a preview_diagnostics retention target",
            ))?;
    let scope = target.access_scope();
    bounded(ids)?;
    if ids.is_empty() {
        return Ok(0);
    }
    Ok(entity::gate_outcome::Entity::delete_many()
        .filter(entity::gate_outcome::Column::OutcomeId.is_in(ids.iter().copied()))
        .filter(entity::gate_outcome::Column::OrderId.is_null())
        .filter(entity::gate_outcome::Column::EvaluatedAt.lt(cutoff))
        .secure()
        .scope_with(scope)
        .exec(tx)
        .await?
        .rows_affected)
}
pub async fn access_log(
    tx: &impl TransactionRunner,
    target: &TargetScope,
) -> Result<u64, ScopeError> {
    let (ids, cutoff) = target
        .retention(RetentionTable::ReadAccessLog)
        .ok_or(ScopeError::Invalid("not a access_log retention target"))?;
    let scope = target.access_scope();
    bounded(ids)?;
    if ids.is_empty() {
        return Ok(0);
    }
    Ok(entity::read_access_log::Entity::delete_many()
        .filter(entity::read_access_log::Column::AccessId.is_in(ids.iter().copied()))
        .filter(entity::read_access_log::Column::AccessedAt.lt(cutoff))
        .secure()
        .scope_with(scope)
        .exec(tx)
        .await?
        .rows_affected)
}
