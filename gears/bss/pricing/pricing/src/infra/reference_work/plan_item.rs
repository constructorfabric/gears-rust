//! The plan item's version of every hook the durable machine dispatches (D-407, D-413, D-414).
//!
//! A created item is written by Tx B, which re-reads its revision (still an unlocked draft) and
//! its entry (of the revision's book, for the item's SKU) in the op's transaction, so SSI orders
//! the write against a submit, a book change or a delete. An attached or rereserved item only has
//! its reference columns moved, in any revision state: the machine is the one writer allowed to
//! touch an item of a published or superseded revision (D-413).
//!
//! @cpt-dod:cpt-cf-bss-pricing-dod-plan-reference-protocol:p1
use super::{
    super::{events::TxOutbox, reference_events},
    Observation, Receipt, Ref, Target, Work, Write, corrupt, new_op,
};
use crate::{
    api::rest::authoring::support::{self, DoorError},
    domain::{
        plan::{MAX_ITEMS, ReferenceState},
        reference_op::{Effect, Event, OpKind, RefKind},
    },
    infra::storage::{
        entity::{plan_item, reference_op as entity},
        repo::{plan_item_repo, plan_revision_repo, reference_op_repo as ops},
    },
};
use bss_products_sdk::models::SkuType;
use time::OffsetDateTime;
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::secure::{AccessScope, DBRunner};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// The local refusals of an item's Tx B that cancel the op and release its reservation: the
/// revision is gone, no longer an unlocked draft or already full, the SKU is already an item of
/// the revision, the entry is missing, of another book or of another SKU, or the attached item is
/// gone.
pub(super) const REFUSES_THE_WRITE: [&str; 8] = [
    "REVISION_NOT_DRAFT",
    "REVISION_NOT_FOUND",
    "REVISION_ITEMS_TOO_MANY",
    "ITEM_SKU_TAKEN",
    "ENTRY_NOT_FOUND",
    "ITEM_BOOK_FOREIGN",
    "ITEM_ENTRY_SKU_MISMATCH",
    "ITEM_NOT_FOUND",
];
/// A bundle SKU is never an item (D-408, D-411); every other type may be one.
#[must_use]
pub const fn type_refusal(sku: SkuType) -> Option<&'static str> {
    match sku {
        SkuType::Bundle => Some("ITEM_BUNDLE_SKU"),
        SkuType::Recurring | SkuType::Usage | SkuType::OneTime => None,
    }
}
/// What Tx B writes once the SKU admits the reference: a create's new item, or the new receipt
/// of an attached or rereserved one.
pub(super) fn written(op: &entity::Model) -> Result<Observation, CanonicalError> {
    let receipt = op.reservation_id.ok_or_else(corrupt)?;
    if op.kind != OpKind::Create.as_str() {
        return Ok((Event::Written, Some(Write::ItemReceipt(receipt)), None));
    }
    let Target::PlanItem {
        revision_id,
        input: Some(input),
    } = Work::read(op)?.target
    else {
        return Err(corrupt());
    };
    let item = plan_item::Model {
        id: op.ref_id,
        tenant_id: op.tenant_id,
        revision_id,
        sku_id: op.sku_id,
        price_book_entry_id: input.price_book_entry_id,
        // `plan_item_repo::insert` writes D-467's shape (`plan::stored_treatment`, no quantity)
        // whatever these carry, and answers the row as stored.
        treatment: String::new(),
        included_qty: None,
        qty_min: None,
        reservation_id: Some(receipt),
        reference_state: ReferenceState::ConfirmationPending.as_str().into(),
        version: 1,
        created_by: op.created_by,
        created_at: op.created_at,
        updated_at: op.updated_at,
    };
    Ok((Event::Written, Some(Write::Item(item)), None))
}
/// Tx B of a create: the insert re-reads the revision and the entry in this transaction, and the
/// revision's items, so two adds racing for its last free place are ordered by the serializable
/// transaction and the loser is refused `REVISION_ITEMS_TOO_MANY`.
pub(super) async fn write(
    tx: &(impl DBRunner + Sync),
    scope: &AccessScope,
    item: plan_item::Model,
) -> Result<(), DoorError> {
    if plan_item_repo::for_revision(tx, scope, item.tenant_id, item.revision_id)
        .await?
        .len()
        >= MAX_ITEMS
    {
        return Err(support::conflict("REVISION_ITEMS_TOO_MANY").into());
    }
    plan_item_repo::insert(tx, scope, item).await?;
    Ok(())
}
/// Tx B of an attach or a rereserve: point the item at its new receipt, whatever the state of
/// its revision. An item removed meanwhile cancels the op, which releases the new receipt.
pub(super) async fn write_receipt(
    tx: &(impl DBRunner + Sync),
    scope: &AccessScope,
    op: &entity::Model,
    receipt: Uuid,
    now: OffsetDateTime,
) -> Result<(), DoorError> {
    let current = plan_item_repo::find(tx, scope, op.tenant_id, op.ref_id)
        .await?
        .ok_or_else(|| support::conflict("ITEM_NOT_FOUND"))?;
    plan_item_repo::set_reference(
        tx,
        scope,
        op.tenant_id,
        op.ref_id,
        current.version,
        ReferenceState::ConfirmationPending,
        Some(receipt),
        now,
    )
    .await?;
    Ok(())
}
/// Tx C. A confirmed receipt confirms the item. A receipt released before its confirm keeps the
/// item `confirmation_pending` and starts a rereserve op in this transaction; that op alone
/// decides between confirmed and lost (D-401).
pub(super) async fn finish_written(
    tx: &(impl DBRunner + Sync),
    ctx: &SecurityContext,
    op: &entity::Model,
    work: &Work,
    effects: &[Effect],
    now: OffsetDateTime,
) -> Result<Receipt, DoorError> {
    let scope = AccessScope::for_tenant(op.tenant_id);
    let mut item = plan_item_repo::find(tx, &scope, op.tenant_id, op.ref_id)
        .await?
        .ok_or_else(corrupt)?;
    if effects.contains(&Effect::Rereserve) {
        // Due at once: no door drives this op, so no in-flight grace applies.
        ops::insert(tx, &scope, rereserve_op(ctx, &item, now, now)?).await?;
        return Ok(Receipt::item(item)?);
    }
    plan_item_repo::set_reference(
        tx,
        &scope,
        op.tenant_id,
        op.ref_id,
        item.version,
        ReferenceState::Confirmed,
        item.reservation_id,
        now,
    )
    .await?;
    item.reference_state = ReferenceState::Confirmed.as_str().into();
    item.version += 1;
    item.updated_at = now;
    support::audit(
        tx,
        ctx,
        work.correlation,
        "plan_item.confirm",
        item.id,
        item.version,
    )
    .await?;
    Ok(Receipt::item(item)?)
}
/// An attach or a rereserve that ended without a live receipt leaves the item lost: the state,
/// the durable `PlanReferenceLost` event and the audit record commit together. A lost item is
/// announced once; a removed one has nothing to lose.
pub(super) async fn mark_lost(
    tx: &(impl DBRunner + Sync),
    outbox: &TxOutbox,
    ctx: &SecurityContext,
    work: &Work,
    op: &entity::Model,
    now: OffsetDateTime,
) -> Result<(), DoorError> {
    let scope = AccessScope::for_tenant(op.tenant_id);
    let Some(mut item) = plan_item_repo::find(tx, &scope, op.tenant_id, op.ref_id).await? else {
        return Ok(());
    };
    if item.reference_state == ReferenceState::Lost.as_str() {
        return Ok(());
    }
    plan_item_repo::set_reference(
        tx,
        &scope,
        op.tenant_id,
        item.id,
        item.version,
        ReferenceState::Lost,
        item.reservation_id,
        now,
    )
    .await?;
    let revision = plan_revision_repo::find(tx, &scope, op.tenant_id, item.revision_id)
        .await?
        .ok_or_else(corrupt)?;
    reference_events::item_lost(outbox, tx, &item, revision.plan_id, ctx.subject_id(), now).await?;
    item.version += 1;
    support::audit(
        tx,
        ctx,
        work.correlation,
        "PlanReferenceLost",
        item.id,
        item.version,
    )
    .await?;
    Ok(())
}
fn reference(item: &plan_item::Model) -> Ref {
    Ref {
        kind: RefKind::PlanItem,
        id: item.id,
        sku_id: item.sku_id,
    }
}
fn work(item: &plan_item::Model, correlation: Uuid) -> Work {
    Work {
        target: Target::PlanItem {
            revision_id: item.revision_id,
            input: None,
        },
        correlation,
        refusal: None,
        receipt: None,
        outcome: None,
        reason: None,
    }
}
/// The attach op of a copied item (D-413), written in the copy's transaction with the item
/// (`unreserved`, no receipt). The door drives it best-effort within the in-flight grace; the
/// ticker finishes the rest, and, unlike a create, may make its first reservation.
/// # Errors
/// Fails only if the durable work record cannot be encoded.
pub fn attach_op(
    ctx: &SecurityContext,
    item: &plan_item::Model,
    correlation: Uuid,
    now: OffsetDateTime,
) -> Result<entity::Model, CanonicalError> {
    let mut op = new_op(
        ctx,
        reference(item),
        &work(item, correlation),
        OpKind::Attach,
        None,
        None,
        now,
    )?;
    op.tenant_id = item.tenant_id;
    Ok(op)
}
/// A rereserve op for a live item, due at `due`.
/// # Errors
/// Fails only if the durable work record cannot be encoded.
pub fn rereserve_op(
    ctx: &SecurityContext,
    item: &plan_item::Model,
    now: OffsetDateTime,
    due: OffsetDateTime,
) -> Result<entity::Model, CanonicalError> {
    let mut op = new_op(
        ctx,
        reference(item),
        &work(item, Uuid::now_v7()),
        OpKind::Rereserve,
        None,
        None,
        now,
    )?;
    op.tenant_id = item.tenant_id;
    op.next_attempt_at = due;
    Ok(op)
}
/// The delete op of a removed item, in `releasing` with the item's receipt, if it has one.
/// # Errors
/// Fails only if the durable work record cannot be encoded.
pub fn delete_op(
    ctx: &SecurityContext,
    item: &plan_item::Model,
    correlation: Uuid,
    now: OffsetDateTime,
) -> Result<entity::Model, CanonicalError> {
    new_op(
        ctx,
        reference(item),
        &work(item, correlation),
        OpKind::Delete,
        item.reservation_id,
        None,
        now,
    )
}
