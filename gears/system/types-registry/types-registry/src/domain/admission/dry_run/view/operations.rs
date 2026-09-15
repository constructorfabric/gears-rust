//! Operation reads use stored rows; terminal item writes are captured for later
//! publication outside the snapshot. Reject acceptance and operation mutations.

use async_trait::async_trait;
use time::OffsetDateTime;
use toolkit_db::DbTx;
use toolkit_db::secure::{AccessScope, ScopeError};
use uuid::Uuid;

use super::{AdmissionView, ItemOutcomeWrite, unsupported};
use crate::domain::admission::fingerprint::ScopeHash;
use crate::domain::ports::{
    ItemSuccess, NewOperation, NewOperationItem, OperationItemRow, OperationRow, OperationStore,
    RecoveryCursor,
};

#[async_trait]
impl OperationStore for AdmissionView {
    async fn find_by_idempotency(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        idempotency_scope_hash: &ScopeHash,
        idempotency_key: &str,
    ) -> Result<Option<OperationRow>, ScopeError> {
        self.base
            .find_by_idempotency(tx, scope, idempotency_scope_hash, idempotency_key)
            .await
    }

    async fn find_by_id(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        id: Uuid,
    ) -> Result<Option<OperationRow>, ScopeError> {
        self.base.find_by_id(tx, scope, id).await
    }

    async fn find_nonterminal_ids(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        after: Option<RecoveryCursor>,
        limit: u64,
    ) -> Result<Vec<RecoveryCursor>, ScopeError> {
        self.base
            .find_nonterminal_ids(tx, scope, after, limit)
            .await
    }

    async fn insert_operation(
        &self,
        _tx: &DbTx<'_>,
        _scope: &AccessScope,
        _new: NewOperation,
    ) -> Result<OperationRow, ScopeError> {
        Err(unsupported(
            "an admission view accepts no operation; acceptance runs against real storage",
        ))
    }

    async fn insert_items(
        &self,
        _tx: &DbTx<'_>,
        _scope: &AccessScope,
        _parent: &OperationRow,
        _items: &[NewOperationItem],
    ) -> Result<(), ScopeError> {
        Err(unsupported(
            "an admission view accepts no operation items; acceptance writes them",
        ))
    }

    async fn find_items(
        &self,
        tx: &DbTx<'_>,
        scope: &AccessScope,
        operation_id: Uuid,
    ) -> Result<Vec<OperationItemRow>, ScopeError> {
        self.base.find_items(tx, scope, operation_id).await
    }

    async fn mark_running(
        &self,
        _tx: &DbTx<'_>,
        _scope: &AccessScope,
        _id: Uuid,
        _now: OffsetDateTime,
    ) -> Result<bool, ScopeError> {
        Err(unsupported(
            "an admission view does not move the operation row; the pass owns that",
        ))
    }

    async fn mark_completed(
        &self,
        _tx: &DbTx<'_>,
        _scope: &AccessScope,
        _id: Uuid,
        _now: OffsetDateTime,
    ) -> Result<bool, ScopeError> {
        Err(unsupported(
            "an admission view does not move the operation row; the pass owns that",
        ))
    }

    async fn mark_abandoned(
        &self,
        _tx: &DbTx<'_>,
        _scope: &AccessScope,
        _id: Uuid,
        _now: OffsetDateTime,
    ) -> Result<bool, ScopeError> {
        Err(unsupported(
            "an admission view does not move the operation row; the pass owns that",
        ))
    }

    /// Capture success and return `true`. The real CAS happens during publication;
    /// virtual effects must remain visible to dependent candidates until then.
    async fn mark_item_succeeded(
        &self,
        _tx: &DbTx<'_>,
        _scope: &AccessScope,
        item_id: i64,
        outcome: ItemSuccess,
        _now: OffsetDateTime,
    ) -> Result<bool, ScopeError> {
        self.overlay()
            .await
            .record_item(item_id, ItemOutcomeWrite::Succeeded(outcome));
        Ok(true)
    }

    async fn mark_item_unchanged(
        &self,
        _tx: &DbTx<'_>,
        _scope: &AccessScope,
        item_id: i64,
        resource_version: i64,
        _now: OffsetDateTime,
    ) -> Result<bool, ScopeError> {
        self.overlay()
            .await
            .record_item(item_id, ItemOutcomeWrite::Unchanged { resource_version });
        Ok(true)
    }

    /// Reject refusal writes: the pass records refusals outside the view.
    async fn mark_item_failed(
        &self,
        _tx: &DbTx<'_>,
        _scope: &AccessScope,
        _item_id: i64,
        _error_payload: String,
        _now: OffsetDateTime,
    ) -> Result<bool, ScopeError> {
        Err(unsupported(
            "an admission view records no refusal; the pass publishes it outside the snapshot",
        ))
    }
}
