//! Persistence contract implemented by each gear inside its own transaction.
use crate::model::{ApprovalError, Decision, ItemRef, Unit, UnitState};
use time::OffsetDateTime;
use toolkit_db::secure::DBRunner;
use uuid::Uuid;
/// Tenant-scoped persistence using the caller's runner for every operation.
///
/// Implementations must preserve typed database errors in [`ApprovalError::Db`]
/// for retry classification. All writes share the caller's transaction; the
/// version compare-and-swap precedes mutations of an existing pending unit.
#[async_trait::async_trait]
pub trait Store<R: DBRunner + Sync>: Send + Sync {
    /// Inserts the unit and its items; replay belongs to the gear's idempotency store.
    ///
    /// # Errors
    /// Returns persistence or scope errors from the gear.
    async fn insert_unit(
        &self,
        runner: &R,
        unit: &Unit,
        items: &[ItemRef],
    ) -> Result<(), ApprovalError>; // replay is the gear's idempotency store, not the unit (spec §2.2)
    /// Finds a unit within the store's tenant scope.
    ///
    /// # Errors
    /// Returns persistence or scope errors from the gear.
    async fn unit(&self, runner: &R, id: Uuid) -> Result<Option<Unit>, ApprovalError>;
    /// `UPDATE … SET version = version + 1 WHERE id = :id AND version = :expected`; `Ok(false)` when nothing matched.
    ///
    /// # Errors
    /// Returns persistence or scope errors from the gear.
    async fn bump_version(
        &self,
        runner: &R,
        id: Uuid,
        expected: i64,
    ) -> Result<bool, ApprovalError>;
    /// Loads the unit's stored items.
    ///
    /// # Errors
    /// Returns persistence or scope errors from the gear.
    async fn items(&self, runner: &R, id: Uuid) -> Result<Vec<ItemRef>, ApprovalError>;
    /// Loads all decisions for this unit, including stale generations.
    ///
    /// # Errors
    /// Returns persistence or scope errors from the gear.
    async fn decisions(&self, runner: &R, id: Uuid) -> Result<Vec<Decision>, ApprovalError>;
    /// The store supplies `tenant_id` for the row from its own scope; `Decision` carries none.
    ///
    /// # Errors
    /// Returns persistence or scope errors from the gear.
    async fn insert_decision(&self, runner: &R, decision: &Decision) -> Result<(), ApprovalError>;
    /// Rewrites items, snapshot and hash, sets `generation`, marks decisions of earlier generations `stale`.
    ///
    /// # Errors
    /// Returns persistence or scope errors from the gear.
    async fn refresh(
        &self,
        runner: &R,
        id: Uuid,
        items: &[ItemRef],
        snapshot: &serde_json::Value,
        snapshot_hash: &str,
        generation: i32,
    ) -> Result<(), ApprovalError>;
    /// Records the terminal state, decision timestamp and optional note.
    ///
    /// # Errors
    /// Returns persistence or scope errors from the gear.
    async fn set_state(
        &self,
        runner: &R,
        id: Uuid,
        state: UnitState,
        decided_at: Option<OffsetDateTime>,
        note: Option<&str>,
    ) -> Result<(), ApprovalError>;
}
