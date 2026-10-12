//! Changes preserve the proposed lifecycle and effective date in review content.
//! @cpt-dod:cpt-cf-bss-products-dod-sku-change-effective-from:p1
//! @cpt-dod:cpt-cf-bss-products-dod-sku-type-frozen:p1
//! @cpt-dod:cpt-cf-bss-products-dod-derived-usage-type-pin:p1
// The subject applies through the repositories, the broker and the outbox.
#![allow(unknown_lints, de0301_no_infra_in_domain)]
use super::{
    KIND_SKU_CHANGE, SkuProposal, apply_error, decode, invalid, json, publish::SkuPublish,
    require_category, sku, store_err,
};
use crate::{
    domain::{
        derived,
        sku::{SkuPatch, apply_patch, changed_fields, lifecycle_edge},
    },
    infra::{broker, events, storage::repo},
};
use bss_approval::{ApprovalError, ApprovalSubject, ItemRef, Unit};
use bss_products_sdk::models::{Lifecycle, SkuContent, SkuType};
use time::Date;
use toolkit_db::DbTx;
use uuid::Uuid;
#[toolkit_macros::domain_model]
#[derive(Clone)]
pub struct SkuChange {
    pub base: SkuPublish,
    pub patch: SkuPatch,
    pub effective_from: Date,
    pub fence_op_id: Option<Uuid>,
}
#[async_trait::async_trait]
impl<'a> ApprovalSubject<DbTx<'a>> for SkuChange {
    fn kind(&self) -> &'static str {
        KIND_SKU_CHANGE
    }
    fn ref_type(&self) -> &'static str {
        "sku"
    }
    async fn collect(&self, tx: &DbTx<'a>, ids: &[Uuid]) -> Result<Vec<ItemRef>, ApprovalError> {
        let mut items = Vec::new();
        for id in ids {
            let s = sku(tx, &self.base.scope, self.base.tenant_id, *id).await?;
            let c = SkuContent::from(&s);
            items.push(ItemRef {
                item_type: "sku".into(),
                item_id: s.id,
                created_by: s.created_by,
                before: Some(json(&SkuProposal {
                    content: c.clone(),
                    lifecycle: None,
                })?),
                after: json(&SkuProposal {
                    content: apply_patch(&c, &self.patch),
                    lifecycle: self.patch.lifecycle,
                })?,
            });
        }
        Ok(items)
    }
    async fn validate_submit(&self, tx: &DbTx<'a>, items: &[ItemRef]) -> Result<(), ApprovalError> {
        let b = &self.base;
        if self.effective_from < b.now.date() {
            return Err(invalid(
                "VALIDATION",
                "effective_from",
                "effective date is in the past",
            ));
        }
        self.validate_change(tx, items).await
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
        serde_json::json!({"skus":items,"effective_from":date.map(|d|d.to_string())})
    }
    async fn apply(
        &self,
        tx: &DbTx<'a>,
        unit: &Unit,
        items: &[ItemRef],
    ) -> Result<(), ApprovalError> {
        self.validate_change(tx, items).await.map_err(apply_error)?;
        if unit.common_effective_date != Some(self.effective_from) {
            return Err(ApprovalError::Store(
                "change effective date disagrees with unit".into(),
            ));
        }
        let b = &self.base;
        let applied_date = self.effective_from.max(b.now.date());
        for i in items {
            let proposal: SkuProposal = decode(&i.after)?;
            let current = sku(tx, &b.scope, b.tenant_id, i.item_id).await?;
            let mut changed = changed_fields(&SkuContent::from(&current), &proposal.content);
            let s = repo::write_sku_content(
                tx,
                &b.scope,
                b.tenant_id,
                i.item_id,
                &proposal.content,
                b.now,
            )
            .await
            .map_err(store_err)?;
            if let Some(target) = proposal.lifecycle {
                let today = b.now.date();
                let written = if target == current.lifecycle {
                    repo::clear_lifecycle_next(tx, &b.scope, b.tenant_id, s.id, b.now).await
                } else if self.effective_from > today {
                    repo::set_lifecycle_next(
                        tx,
                        &b.scope,
                        b.tenant_id,
                        s.id,
                        &[current.lifecycle],
                        target,
                        self.effective_from,
                        b.now,
                    )
                    .await
                } else {
                    repo::set_lifecycle(
                        tx,
                        &b.scope,
                        b.tenant_id,
                        s.id,
                        &[current.lifecycle],
                        target,
                        b.now,
                    )
                    .await
                };
                if matches!(written.map_err(store_err)?, repo::HeadWrite::Unmatched) {
                    return Err(apply_error(invalid(
                        "ILLEGAL_TRANSITION",
                        "lifecycle",
                        "lifecycle changed",
                    )));
                }
                changed.push("lifecycle".into());
                changed.sort();
            }
            repo::append_version(
                tx,
                &b.scope,
                b.tenant_id,
                s.id,
                s.published_version,
                applied_date,
                &proposal.content,
                b.now,
            )
            .await
            .map_err(store_err)?;
            events::enqueue_typed(
                &b.outbox,
                tx,
                broker::SkuChanged {
                    tenant_id: b.tenant_id,
                    sku_id: s.id,
                    changed,
                    effective_from: applied_date,
                    published_version: s.published_version,
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
        if let Some(op) = self.fence_op_id {
            for i in items {
                if matches!(
                    repo::unlock_and_unfence(
                        tx,
                        &b.scope,
                        b.tenant_id,
                        i.item_id,
                        unit.id,
                        op,
                        approved.then_some(unit.id),
                        None
                    )
                    .await
                    .map_err(store_err)?,
                    repo::HeadWrite::Unmatched
                ) {
                    return Err(ApprovalError::Contended);
                }
            }
            Ok(())
        } else {
            b.unlock(tx, unit, items, approved).await
        }
    }
}

/// Recheck durable content and fence constraints at both submit and apply.
impl SkuChange {
    pub(crate) async fn validate_change(
        &self,
        tx: &DbTx<'_>,
        items: &[ItemRef],
    ) -> Result<(), ApprovalError> {
        let b = &self.base;
        for i in items {
            let s = sku(tx, &b.scope, b.tenant_id, i.item_id).await?;
            if !matches!(s.lifecycle, Lifecycle::Published | Lifecycle::Deprecated) {
                return Err(invalid(
                    "ILLEGAL_TRANSITION",
                    "lifecycle",
                    "only published or deprecated SKUs can change",
                ));
            }
            let proposed: SkuProposal = decode(&i.after)?;
            if let Some(target) = proposed.lifecycle
                && target != s.lifecycle
                && (!matches!(target, Lifecycle::Published | Lifecycle::Deprecated)
                    || !lifecycle_edge(s.lifecycle, target))
            {
                return Err(invalid(
                    "ILLEGAL_TRANSITION",
                    "lifecycle",
                    "use the fenced retire operation to retire",
                ));
            }
            // P-D-258, the one exception P-D-251: a published usage SKU keeps its ref and its unit,
            // except a raw meter moving onto the identity wrapper of that meter, in the same unit.
            // Judged against the head as it is now, at submit and again at apply, before the fence
            // and type checks. The version is the same tenant-scoped read the change door uses.
            let current_ref = s.usage_type_ref.as_deref();
            let proposed_ref = proposed.content.usage_type_ref.as_deref();
            let stored = match proposed_ref {
                Some(reference) if derived::wrap_candidate(current_ref, Some(reference)) => {
                    let read_scope = b.scope.tenant_only();
                    crate::api::rest::derived_usage_types::stored_declaration(
                        tx,
                        &read_scope,
                        b.tenant_id,
                        reference,
                    )
                    .await
                    .map_err(store_err)?
                }
                _ => None,
            };
            if let Some(field) = derived::metering_moves(
                derived::Metering {
                    usage: s.r#type == SkuType::Usage,
                    usage_type_ref: current_ref,
                    unit: s.unit.as_deref(),
                },
                derived::Metering {
                    usage: proposed.content.r#type == SkuType::Usage,
                    usage_type_ref: proposed_ref,
                    unit: proposed.content.unit.as_deref(),
                },
                stored.as_ref(),
            ) {
                return Err(invalid(
                    derived::METERING_IMMUTABLE,
                    field,
                    derived::METERING_DETAIL,
                ));
            }
            if proposed.content.r#type != s.r#type || self.fence_op_id.is_some() {
                let fence = repo::find_sku_fence(tx, &b.scope, b.tenant_id, s.id)
                    .await
                    .map_err(store_err)?;
                if !s.type_change_pending
                    || self.fence_op_id.is_none()
                    || fence.as_ref().and_then(|f| f.fence_op_id) != self.fence_op_id
                {
                    return Err(invalid(
                        "SKU_FENCED",
                        "type",
                        "type change requires its matching fence",
                    ));
                }
                if !repo::live_references(tx, &b.scope, b.tenant_id, s.id)
                    .await
                    .map_err(store_err)?
                    .is_empty()
                {
                    return Err(invalid(
                        "SKU_TYPE_FROZEN",
                        "type",
                        "live references prevent type changes",
                    ));
                }
            }
            // P-D-196: a category the change sets is resolved in tenant scope and must be active,
            // at submit and again at apply, as the draft doors resolve it: 404 when the tenant
            // does not hold it, 409 `CATEGORY_RETIRED` when it is retired.
            if let Some(category) = proposed.content.category_id
                && Some(category) != s.category_id
            {
                require_category(tx, &b.scope, b.tenant_id, category).await?;
            }
            b.validate_content(&proposed.content)?;
        }
        Ok(())
    }
}
