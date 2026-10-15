//! Bounded internal maintenance authority (08 §3.5, D-184).
//!
//! Only lifecycle-owned workers reach this module; it is not exported and no REST/SDK path
//! accepts its types. Worker bodies (expiry, auto-void, cleanup, purge, verification and
//! checkpointing) are delivered by S2-11/S5 on these capability types.
use crate::infra::storage::entity;
use crate::infra::storage::repo::{LockedOrder, TransactionRunner};
use toolkit_db::secure::ScopeError;

pub mod scope;
pub use scope::{
    MaintenanceAuthority, MaintenanceTask, RetentionTable, ServiceActor, TargetScope, TaskGrant,
};

/// The private worker engine entry: lock the discovered order under its narrowed target scope
/// and recheck every discovered persisted fact under the aggregate lock. `None` means the
/// candidate changed or vanished; the worker skips it rather than widening scope.
///
/// # Errors
/// `Invalid` for a non-order target; scope/store failure.
pub async fn lock_target_order<'a, T: TransactionRunner>(
    tx: &'a T,
    target: &TargetScope,
) -> Result<Option<LockedOrder<'a, T>>, ScopeError> {
    let Some(discovered) = target.order() else {
        return Err(ScopeError::Invalid("not an order maintenance target"));
    };
    let locked =
        LockedOrder::lock_current(tx, target.access_scope(), discovered.order_id()).await?;
    Ok(locked.filter(|locked| discovered.unchanged(locked.row())))
}
