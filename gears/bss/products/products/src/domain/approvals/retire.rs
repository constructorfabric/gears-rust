//! Retirement retains its fence until the owning unit atomically unlocks it.
// The subject applies through the repositories, the broker and the outbox.
#![allow(unknown_lints, de0301_no_infra_in_domain)]
use super::{KIND_SKU_RETIRE, apply_error, invalid, publish::SkuPublish, store_err};
use crate::infra::{broker, events, storage::repo};
use bss_approval::{ApprovalError, ApprovalSubject, ItemRef, Unit};
use time::Date;
use toolkit_db::DbTx;
use uuid::Uuid;
#[toolkit_macros::domain_model]
#[derive(Clone)]
pub struct SkuRetire {
    pub base: SkuPublish,
    pub fence_op_id: Uuid,
}
#[async_trait::async_trait]
impl<'a> ApprovalSubject<DbTx<'a>> for SkuRetire {
    fn kind(&self) -> &'static str {
        KIND_SKU_RETIRE
    }
    fn ref_type(&self) -> &'static str {
        "sku"
    }
    async fn collect(&self, tx: &DbTx<'a>, ids: &[Uuid]) -> Result<Vec<ItemRef>, ApprovalError> {
        self.base.collect(tx, ids).await
    }
    async fn validate_submit(&self, tx: &DbTx<'a>, items: &[ItemRef]) -> Result<(), ApprovalError> {
        let b = &self.base;
        for i in items {
            let s = repo::find_sku_fence(tx, &b.scope, b.tenant_id, i.item_id)
                .await
                .map_err(store_err)?
                .ok_or_else(|| invalid("NOT_FOUND", "id", i.item_id.to_string()))?;
            if !s.retire_pending || s.fence_op_id != Some(self.fence_op_id) {
                return Err(invalid(
                    "SKU_FENCED",
                    "lifecycle",
                    "retirement requires its matching fence",
                ));
            }
        }
        Ok(())
    }
    async fn lock(
        &self,
        tx: &DbTx<'a>,
        unit: Uuid,
        items: &[ItemRef],
    ) -> Result<(), ApprovalError> {
        self.base.lock_items(tx, unit, items).await
    }
    fn snapshot(&self, items: &[ItemRef], date: Option<Date>) -> serde_json::Value {
        self.base.snapshot(items, date)
    }
    async fn apply(&self, tx: &DbTx<'a>, _: &Unit, items: &[ItemRef]) -> Result<(), ApprovalError> {
        self.validate_submit(tx, items).await.map_err(apply_error)?;
        let b = &self.base;
        for i in items {
            if !repo::live_references(tx, &b.scope, b.tenant_id, i.item_id)
                .await
                .map_err(store_err)?
                .is_empty()
            {
                return Err(ApprovalError::ApplyRefused {
                    code: "SKU_REFERENCED",
                    detail: "live references prevent retirement".into(),
                });
            }
            events::enqueue_typed(
                &b.outbox,
                tx,
                broker::SkuRetired {
                    tenant_id: b.tenant_id,
                    sku_id: i.item_id,
                    actor_ref: b.actor,
                },
            )
            .await
            .map_err(ApprovalError::from)?;
        }
        Ok(())
    }
    async fn unlock(
        &self,
        tx: &DbTx<'a>,
        unit: &Unit,
        items: &[ItemRef],
        approved: bool,
    ) -> Result<(), ApprovalError> {
        let b = &self.base;
        for i in items {
            if matches!(
                repo::unlock_and_unfence(
                    tx,
                    &b.scope,
                    b.tenant_id,
                    i.item_id,
                    unit.id,
                    self.fence_op_id,
                    approved.then_some(unit.id),
                    approved.then_some(b.now)
                )
                .await
                .map_err(store_err)?,
                repo::HeadWrite::Unmatched
            ) {
                return Err(ApprovalError::Contended);
            }
        }
        Ok(())
    }
}
