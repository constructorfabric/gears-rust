//! @cpt-dod:cpt-cf-bss-products-dod-sku-publish-unit:p1
//! Publishing a draft with a business-only review fingerprint.
// The subject applies through the repositories, the broker and the outbox.
#![allow(unknown_lints, de0301_no_infra_in_domain)]
use super::{KIND_SKU_PUBLISH, apply_error, invalid, json, sku, store_err};
use crate::{
    domain::{recognized::UsageRefAnswer, sku::validate_publish},
    infra::{
        broker,
        events::{self, TxOutbox},
        storage::repo,
    },
};
use bss_approval::{ApprovalError, ApprovalSubject, ItemRef, Unit};
use bss_products_sdk::models::{Lifecycle, SkuContent};
use time::{Date, OffsetDateTime};
use toolkit_db::{DbTx, secure::AccessScope};
use uuid::Uuid;

#[toolkit_macros::domain_model]
#[derive(Clone)]
pub struct SkuPublish {
    pub scope: AccessScope,
    pub tenant_id: Uuid,
    /// The attempt's handle (P-D-221): the events `apply` enqueues wake the outbox's sequencer
    /// once the door's transaction commits.
    pub outbox: TxOutbox,
    pub actor: Uuid,
    pub now: OffsetDateTime,
    /// What the door resolved the content's `usage_type_ref` to before the transaction: the
    /// catalog's answer for a GTS ref, the tenant's stored version for a derived one (P-D-232).
    pub usage_type: Option<UsageRefAnswer>,
}
impl SkuPublish {
    pub(super) async fn lock_items(
        &self,
        tx: &DbTx<'_>,
        unit_id: Uuid,
        items: &[ItemRef],
    ) -> Result<(), ApprovalError> {
        for i in items {
            let s = sku(tx, &self.scope, self.tenant_id, i.item_id).await?;
            if !repo::try_lock_sku(tx, &self.scope, self.tenant_id, s.id, unit_id, s.revision)
                .await
                .map_err(store_err)?
            {
                return Err(ApprovalError::Locked {
                    item_type: "sku".into(),
                    item_id: s.id,
                });
            }
        }
        Ok(())
    }
    pub(super) fn validate_content(&self, content: &SkuContent) -> Result<(), ApprovalError> {
        if let Some(v) = validate_publish(content, self.usage_type.as_ref())
            .violations()
            .first()
        {
            return Err(invalid(v.code, &v.subject, v.detail.clone()));
        }
        if content.name.trim().is_empty() {
            return Err(invalid("VALIDATION", "name", "name must not be blank"));
        }
        Ok(())
    }
}
#[async_trait::async_trait]
impl<'a> ApprovalSubject<DbTx<'a>> for SkuPublish {
    fn kind(&self) -> &'static str {
        KIND_SKU_PUBLISH
    }
    fn ref_type(&self) -> &'static str {
        "sku"
    }
    async fn collect(&self, tx: &DbTx<'a>, ids: &[Uuid]) -> Result<Vec<ItemRef>, ApprovalError> {
        let mut items = Vec::new();
        for id in ids {
            let s = sku(tx, &self.scope, self.tenant_id, *id).await?;
            items.push(ItemRef {
                item_type: "sku".into(),
                item_id: s.id,
                created_by: s.created_by,
                before: None,
                after: json(&SkuContent::from(&s))?,
            });
        }
        Ok(items)
    }
    async fn validate_submit(&self, tx: &DbTx<'a>, items: &[ItemRef]) -> Result<(), ApprovalError> {
        for i in items {
            let s = sku(tx, &self.scope, self.tenant_id, i.item_id).await?;
            if s.lifecycle != Lifecycle::Draft {
                return Err(invalid(
                    "NOT_A_DRAFT",
                    "lifecycle",
                    "only a draft publishes",
                ));
            }
            self.validate_content(&SkuContent::from(&s))?;
        }
        Ok(())
    }
    async fn lock(
        &self,
        tx: &DbTx<'a>,
        unit: Uuid,
        items: &[ItemRef],
    ) -> Result<(), ApprovalError> {
        self.lock_items(tx, unit, items).await
    }
    fn snapshot(&self, items: &[ItemRef], _: Option<Date>) -> serde_json::Value {
        serde_json::json!({"skus":items.iter().map(|i| &i.after).collect::<Vec<_>>()})
    }
    async fn apply(&self, tx: &DbTx<'a>, _: &Unit, items: &[ItemRef]) -> Result<(), ApprovalError> {
        self.validate_submit(tx, items).await.map_err(apply_error)?;
        for i in items {
            let s = match repo::set_lifecycle(
                tx,
                &self.scope,
                self.tenant_id,
                i.item_id,
                &[Lifecycle::Draft],
                Lifecycle::Published,
                self.now,
            )
            .await
            .map_err(store_err)?
            {
                repo::HeadWrite::Written(s) => s,
                repo::HeadWrite::Unmatched => {
                    return Err(apply_error(invalid(
                        "NOT_A_DRAFT",
                        "lifecycle",
                        "draft changed",
                    )));
                }
            };
            let c = SkuContent::from(&s);
            let s = repo::write_sku_content(tx, &self.scope, self.tenant_id, s.id, &c, self.now)
                .await
                .map_err(store_err)?;
            repo::append_version(
                tx,
                &self.scope,
                self.tenant_id,
                s.id,
                s.published_version,
                self.now.date(),
                &c,
                self.now,
            )
            .await
            .map_err(store_err)?;
            events::enqueue_typed(
                &self.outbox,
                tx,
                broker::SkuPublished {
                    tenant_id: self.tenant_id,
                    sku_id: s.id,
                    published_version: s.published_version,
                    actor_ref: self.actor,
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
        for i in items {
            if matches!(
                repo::unlock_sku(
                    tx,
                    &self.scope,
                    self.tenant_id,
                    i.item_id,
                    unit.id,
                    approved.then_some(unit.id),
                )
                .await
                .map_err(store_err)?,
                repo::HeadWrite::Unmatched
            ) {
                return Err(ApprovalError::Store(
                    "SKU pending lock is not owned by this unit".into(),
                ));
            }
        }
        Ok(())
    }
}
