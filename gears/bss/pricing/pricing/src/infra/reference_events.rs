//! Durable lost-reference events through the shared pricing event writer: an entry's
//! `PriceBookEntryReferenceLost` and a plan item's `PlanReferenceLost` (D-407).
use super::{
    events::{self, SOURCE},
    storage::{
        RepoError,
        entity::{plan_item, price_book_entry},
    },
};
use event_broker_sdk::TypedEvent;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use toolkit_db::secure::DBRunner;
use toolkit_gts::gts_id;
use uuid::Uuid;
// @cpt-begin:cpt-cf-bss-pricing-algo-read-contract-events-typed-events:p1:inst-read-contract-events-typed-events-1
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceBookEntryReferenceLost {
    pub tenant_id: Uuid,
    pub price_book_entry_id: Uuid,
    pub sku_id: Uuid,
    pub reservation_id: Uuid,
    pub actor_ref: Uuid,
}
impl TypedEvent for PriceBookEntryReferenceLost {
    const TYPE_ID: &'static str =
        gts_id!("cf.core.events.event.v1~cf.bss.pricing.price_book_entry_reference_lost.v1~");
    const SUBJECT_TYPE: &'static str =
        gts_id!("cf.core.events.subject.v1~cf.bss.pricing.price_book_entry.v1~");
    const SOURCE: &'static str = SOURCE;
    fn subject(&self) -> Cow<'_, str> {
        Cow::Owned(self.price_book_entry_id.to_string())
    }
    fn tenant_id(&self) -> Option<Uuid> {
        Some(self.tenant_id)
    }
}
// @cpt-end:cpt-cf-bss-pricing-algo-read-contract-events-typed-events:p1:inst-read-contract-events-typed-events-1
/// Enqueue on the same runner as the entry/op transition, so rollback erases the event.
/// # Errors
/// Preserves typed database errors for serializable transaction retries.
pub async fn lost(
    outbox: &events::TxOutbox,
    tx: &(impl DBRunner + Sync),
    entry: &price_book_entry::Model,
    actor: Uuid,
    now: time::OffsetDateTime,
) -> Result<(), RepoError> {
    let event = PriceBookEntryReferenceLost {
        tenant_id: entry.tenant_id,
        price_book_entry_id: entry.id,
        sku_id: entry.sku_id,
        reservation_id: entry.reservation_id,
        actor_ref: actor,
    };
    events::enqueue(outbox, tx, &event, now).await
}

// @cpt-begin:cpt-cf-bss-pricing-algo-read-contract-events-typed-events:p1:inst-read-contract-events-typed-events-2
/// A plan item's reference is lost: its attach or rereserve ended without a live receipt, so
/// the checks show `ITEM_REFERENCE_LOST` until reconciliation re-reserves it. It is about the
/// item and names its plan and revision.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanReferenceLost {
    pub tenant_id: Uuid,
    pub plan_id: Uuid,
    pub revision_id: Uuid,
    pub item_id: Uuid,
    pub sku_id: Uuid,
    /// The receipt the item last held; none when a copied item never attached (D-413).
    pub reservation_id: Option<Uuid>,
    pub actor_ref: Uuid,
}
impl TypedEvent for PlanReferenceLost {
    const TYPE_ID: &'static str =
        gts_id!("cf.core.events.event.v1~cf.bss.pricing.plan_reference_lost.v1~");
    const SUBJECT_TYPE: &'static str =
        gts_id!("cf.core.events.subject.v1~cf.bss.pricing.plan_item.v1~");
    const SOURCE: &'static str = SOURCE;
    fn subject(&self) -> Cow<'_, str> {
        Cow::Owned(self.item_id.to_string())
    }
    fn tenant_id(&self) -> Option<Uuid> {
        Some(self.tenant_id)
    }
}
// @cpt-end:cpt-cf-bss-pricing-algo-read-contract-events-typed-events:p1:inst-read-contract-events-typed-events-2
/// Enqueue on the same runner as the item/op transition, so rollback erases the event.
/// # Errors
/// Preserves typed database errors for serializable transaction retries.
pub async fn item_lost(
    outbox: &events::TxOutbox,
    tx: &(impl DBRunner + Sync),
    item: &plan_item::Model,
    plan_id: Uuid,
    actor: Uuid,
    now: time::OffsetDateTime,
) -> Result<(), RepoError> {
    let event = PlanReferenceLost {
        tenant_id: item.tenant_id,
        plan_id,
        revision_id: item.revision_id,
        item_id: item.id,
        sku_id: item.sku_id,
        reservation_id: item.reservation_id,
        actor_ref: actor,
    };
    events::enqueue(outbox, tx, &event, now).await
}
