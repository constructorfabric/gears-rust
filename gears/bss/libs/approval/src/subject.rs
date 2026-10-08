//! Business operations supplied by a gear for one approval kind.
use crate::model::{ApprovalError, ItemRef, Unit};
use time::Date;
use toolkit_db::secure::DBRunner;
use uuid::Uuid;
/// Collects, locks and applies business content within the caller's transaction.
///
/// The gear owns authorization and scope, and must use the supplied runner for
/// persistence. Lock acquisition is conditional; application revalidates the
/// environment and publishes domain effects in that same transaction.
///
/// An effect that may act only after the commit (an outbox wake) never passes
/// through the engine: the gear builds the subject for its transaction, the
/// subject keeps what `apply` produced, and the gear acts on it once the
/// transaction has committed (pricing D-455, products P-D-221).
#[async_trait::async_trait]
pub trait ApprovalSubject<R: DBRunner + Sync>: Send + Sync {
    /// The approval kind used for policy lookup.
    fn kind(&self) -> &'static str;
    /// The type of aggregate referenced by a unit.
    fn ref_type(&self) -> &'static str;
    /// Items with `after` = proposed business content only (see `ItemRef`).
    ///
    /// # Errors
    /// Returns validation, locking, application or persistence errors from the gear.
    async fn collect(&self, runner: &R, ids: &[Uuid]) -> Result<Vec<ItemRef>, ApprovalError>;
    /// Checks that every collected item can be submitted.
    ///
    /// # Errors
    /// Returns validation, locking, application or persistence errors from the gear.
    async fn validate_submit(&self, runner: &R, items: &[ItemRef]) -> Result<(), ApprovalError>;
    /// Conditional lock per item; zero rows → `ApprovalError::Locked`.
    ///
    /// # Errors
    /// Returns validation, locking, application or persistence errors from the gear.
    async fn lock(&self, runner: &R, unit_id: Uuid, items: &[ItemRef])
    -> Result<(), ApprovalError>;
    /// Builds the reviewer-facing snapshot; the fingerprint is computed separately.
    fn snapshot(&self, items: &[ItemRef], common_effective_date: Option<Date>)
    -> serde_json::Value;
    /// Publishes. Re-validates; an environment change (a name taken meanwhile) is `ApplyRefused { code, detail }`.
    ///
    /// # Errors
    /// Returns validation, locking, application or persistence errors from the gear.
    async fn apply(&self, runner: &R, unit: &Unit, items: &[ItemRef]) -> Result<(), ApprovalError>;
    /// Clears the lock (reject, withdraw) or turns it into `approved_by_unit_id` (approve).
    ///
    /// # Errors
    /// Returns validation, locking, application or persistence errors from the gear.
    async fn unlock(
        &self,
        runner: &R,
        unit: &Unit,
        items: &[ItemRef],
        approved: bool,
    ) -> Result<(), ApprovalError>;
}
