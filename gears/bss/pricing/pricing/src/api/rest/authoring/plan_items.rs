//! The plan item's doors and its operations on the durable reference machine (D-404, D-407,
//! D-413, D-414).
//!
//! `POST /plan-revisions/{id}/items` ([`add`]) judges its input, the revision (an unlocked draft
//! of the caller), the entry, the revision's items and the SKU read fresh, all before any claim or
//! reservation (a refusal is 400 and costs nothing, D-403); then [`create`] writes the create op
//! and drives it. `PATCH /plan-items/{id}` (`patch`) edits an item of an unlocked draft of the
//! caller, never its SKU. `DELETE /plan-items/{id}` ([`delete`]) removes it with its delete op;
//! a draft revision's delete removes every item through `remove`. A copy or a clone writes each
//! copied item `unreserved` with its attach op ([`reference_work::attach_op`]) in its own
//! transaction and drives them with [`drive_best_effort`].
use super::{
    AuthoringState,
    dto::{PricingPlanItemCreate, PricingPlanItemDto, PricingPlanItemPatch},
    plans,
    price_book_entries::{settled, stored},
    support::{self, DoorError},
};
use crate::api::rest::closed_sets::PricingRevisionState;
use crate::{
    domain::{
        plan::{MAX_ITEMS, ReferenceState, RevisionState},
        reference_op::{OpKind, RefKind},
    },
    infra::{
        reference_registry,
        reference_work::{self, Caller, Receipt, Ref, Target, WallClock, Work},
        storage::{
            entity::plan_revision,
            repo::{
                idempotency_repo as idem, plan_item_repo, plan_revision_repo,
                price_book_entry_repo, reference_op_repo,
            },
        },
    },
};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use bss_products_sdk::models::Lifecycle;
use std::sync::Arc;
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::secure::{AccessScope, DBRunner};
use toolkit_security::SecurityContext;
use uuid::Uuid;
/// The keys a plan item no longer takes (D-467): a plan item is a SKU and its entry.
pub const REMOVED_KEYS: [&str; 3] = ["treatment", "included_qty", "qty_min"];
/// The item create's body rule that the typed body cannot say, judged before any read: a key a
/// plan item no longer takes is 400 `BODY_UNEXPECTED` on that key, the rule for a stray key
/// (D-467). An absent or null `price_book_entry_id` adds an entry-less item (D-512). Any other key
/// the body does not know is refused by its parse, as before.
/// # Errors
/// The refusals above.
pub fn judge_create_body(body: &serde_json::Value) -> Result<(), CanonicalError> {
    let Some(fields) = body.as_object() else {
        return Ok(());
    };
    refuse_removed_keys(fields)
}
/// The item PATCH's body rule that the typed body cannot say, judged before any read: a key a
/// plan item no longer takes is 400 `BODY_UNEXPECTED` on that key (D-467). Its entry is judged
/// against the stored item, by the PATCH below its door (`patch`).
/// # Errors
/// The refusal above.
pub fn judge_patch_body(body: &serde_json::Value) -> Result<(), CanonicalError> {
    body.as_object().map_or(Ok(()), refuse_removed_keys)
}
/// A key a plan item no longer takes is 400 `BODY_UNEXPECTED` on that key (D-467).
fn refuse_removed_keys(
    fields: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), CanonicalError> {
    match REMOVED_KEYS.iter().find(|key| fields.contains_key(**key)) {
        Some(key) => Err(support::invalid_because(
            key,
            "BODY_UNEXPECTED",
            "a plan item is a SKU and its entry: it takes no treatment, included_qty or qty_min",
        )),
        None => Ok(()),
    }
}
/// Every item points at a price (D-467): an item without an entry is a legacy row, and a PATCH
/// that keeps it without one, or clears its entry, is refused.
fn entry_needed(entry: Option<Uuid>) -> Result<(), DoorError> {
    if entry.is_none() {
        return Err(support::invalid("price_book_entry_id", "ITEM_ENTRY_MISSING").into());
    }
    Ok(())
}
/// The entry must exist, belong to the revision's book, price the item's SKU and hold its
/// reference (D-522: `BOOK_ARCHIVED`, `ENTRY_REFERENCE_RELEASED`).
async fn entry_fits(
    tx: &impl DBRunner,
    scope: &AccessScope,
    revision: &plan_revision::Model,
    sku: Uuid,
    entry: Uuid,
) -> Result<(), DoorError> {
    let e = price_book_entry_repo::find(tx, scope, revision.tenant_id, entry)
        .await?
        .ok_or_else(support::missing_entry)?;
    if e.book_id != revision.book_id {
        return Err(support::invalid("price_book_entry_id", "ITEM_BOOK_FOREIGN").into());
    }
    if e.sku_id != sku {
        return Err(support::invalid("price_book_entry_id", "ITEM_ENTRY_SKU_MISMATCH").into());
    }
    // D-522: no item names an entry of an archived book, or one not re-reserved since.
    support::writable_entry(tx, &e).await
}

/// `POST /plan-revisions/{id}/items` below its door: a replay answers from the key's store; then
/// every refusal the door owns is judged before anything is claimed or reserved, and only then
/// does [`create`] write its op and drive it (D-401, D-407).
/// # Errors
/// 400 `ITEM_BOOK_FOREIGN`, `ITEM_ENTRY_SKU_MISMATCH`, `REVISION_ITEMS_TOO_MANY`,
/// `ITEM_SKU_DEPRECATED` (a deprecated SKU the plan's published revision in effect does not
/// carry, D-465) or `ITEM_BUNDLE_SKU`; 404 for an unknown revision or entry; 409
/// `REVISION_NOT_DRAFT` or `ITEM_SKU_TAKEN`; 403 `NOT_DRAFT_AUTHOR` (D-404); 503 when Products
/// cannot answer; then [`create`]'s own.
#[expect(
    clippy::too_many_arguments,
    reason = "authorized door identity and replay operands"
)]
pub async fn add(
    state: Arc<AuthoringState>,
    scope: AccessScope,
    ctx: SecurityContext,
    revision: Uuid,
    correlation: Uuid,
    key: String,
    digest: Vec<u8>,
    input: PricingPlanItemCreate,
) -> Result<Response, CanonicalError> {
    let endpoint = format!("/bss-pricing/v1/plan-revisions/{revision}/items");
    if let Some(receipt) = stored(&state, ctx.subject_tenant_id(), &endpoint, &key, &digest).await?
    {
        return receipt.response();
    }
    let (judged_scope, judged_ctx, judged) = (scope.clone(), ctx.clone(), input.clone());
    let admitted = support::transaction(&state.db.db(), move |tx| {
        let (scope, ctx, input) = (judged_scope.clone(), judged_ctx.clone(), judged.clone());
        Box::pin(async move { admit(tx, &scope, &ctx, revision, &input).await })
    })
    .await?;
    // @cpt-begin:cpt-cf-bss-pricing-flow-plans:p1:inst-plans-flow-2
    fresh_sku(&state, &ctx, input.sku_id, admitted).await?;
    create(state, scope, ctx, revision, correlation, key, digest, input).await
    // @cpt-end:cpt-cf-bss-pricing-flow-plans:p1:inst-plans-flow-2
}
/// What an item create's read admits, beyond its refusals: whether the plan's published revision
/// in effect carries the item's SKU (D-465), which admits a deprecated one (the phase 9 review's
/// R11: a named field, never a bare `bool` that reads as "admissible").
#[derive(Debug, Clone, Copy)]
struct Admitted {
    sku_carried: bool,
}
/// The revision, the entry and the revision's items, judged in one read: each refusal of the
/// create's own, then what the read admits.
async fn admit(
    tx: &impl DBRunner,
    scope: &AccessScope,
    ctx: &SecurityContext,
    revision: Uuid,
    input: &PricingPlanItemCreate,
) -> Result<Admitted, DoorError> {
    let tenant = ctx.subject_tenant_id();
    let children = AccessScope::for_tenant(tenant);
    let r = plans::find_revision(tx, scope, tenant, revision).await?;
    plans::editable(&r, ctx)?;
    if let Some(entry) = input.price_book_entry_id {
        entry_fits(tx, &children, &r, input.sku_id, entry).await?;
    }
    let items = plan_item_repo::for_revision(tx, &children, tenant, revision).await?;
    if items.iter().any(|i| i.sku_id == input.sku_id) {
        return Err(support::conflict("ITEM_SKU_TAKEN").into());
    }
    if items.len() >= MAX_ITEMS {
        return Err(support::invalid("items", "REVISION_ITEMS_TOO_MANY").into());
    }
    let sku_carried =
        crate::infra::plan_revisions::published_skus(tx, tenant, r.plan_id, plans::today())
            .await?
            .contains(&input.sku_id);
    Ok(Admitted { sku_carried })
}
/// The SKU read fresh (D-408): a deprecated SKU is added only when the plan's published revision
/// in effect carries it (`admitted.sku_carried`: a re-add is not "newly added", D-465), and a
/// bundle SKU is never an item. A registry that cannot answer is 503 with nothing written; a definite Products
/// refusal is answered as Products gave it.
async fn fresh_sku(
    state: &AuthoringState,
    ctx: &SecurityContext,
    sku: Uuid,
    admitted: Admitted,
) -> Result<(), CanonicalError> {
    let registry =
        reference_registry::resolve(&state.hub).map_err(|e| support::registry_unavailable(&e))?;
    let sku = registry
        .sku_for_write(ctx, ctx.subject_tenant_id(), sku)
        .await
        .map_err(|error| {
            if reference_work::definite_refusal(&error) {
                error
            } else {
                support::registry_unavailable(&error)
            }
        })?;
    if sku.lifecycle == Lifecycle::Deprecated && !admitted.sku_carried {
        return Err(support::invalid("sku_id", "ITEM_SKU_DEPRECATED"));
    }
    if let Some(code) = reference_work::plan_item::type_refusal(sku.r#type) {
        return Err(support::invalid("sku_id", code));
    }
    Ok(())
}
/// `PATCH /plan-items/{id}` below its door: the entry of an item of an unlocked draft of the
/// caller, at the version the caller read; the SKU never changes. The row is written in the shape
/// of D-467 (`paid`, no quantity), so a legacy item that is given an entry stops being one.
/// # Errors
/// 404; 409 `REVISION_NOT_DRAFT`; 403 `NOT_DRAFT_AUTHOR`; 409 `STALE_REVISION`; 400
/// `ITEM_ENTRY_MISSING` for an item left without an entry; the entry refusals of [`add`].
pub(super) async fn patch(
    tx: &impl DBRunner,
    scope: &AccessScope,
    ctx: &SecurityContext,
    correlation: Uuid,
    id: Uuid,
    version: u64,
    input: PricingPlanItemPatch,
) -> Result<Response, DoorError> {
    let tenant = ctx.subject_tenant_id();
    let children = AccessScope::for_tenant(tenant);
    let mut m = plan_item_repo::find(tx, scope, tenant, id)
        .await?
        .ok_or_else(|| support::missing_what("plan_item"))?;
    let r = plans::find_revision(tx, &children, tenant, m.revision_id).await?;
    plans::editable(&r, ctx)?;
    support::check_version(version, m.version)?;
    if let Some(entry) = input.price_book_entry_id {
        if let Some(entry) = entry {
            entry_fits(tx, &children, &r, m.sku_id, entry).await?;
        }
        m.price_book_entry_id = entry;
    }
    entry_needed(m.price_book_entry_id)?;
    m.updated_at = crate::infra::storage::stored_now();
    // The repository rewrites the row in D-467's shape (`plan_item_repo::update_draft`).
    plan_item_repo::update_draft(tx, &children, m.clone()).await?;
    m.version += 1;
    support::audit(tx, ctx, correlation, "plan_item.patch", id, m.version).await?;
    Ok(support::response(
        StatusCode::OK,
        &PricingPlanItemDto::try_from(m)?,
        Some(version + 1),
    )?)
}
/// `GET /plan-items/{id}` (D-434): the item with its revision's number and state and its plan,
/// its version as the `ETag` a following PATCH sends back as If-Match. The state is the one the
/// revision reads today among its plan's revisions (D-447). The door names its actors (D-519).
/// # Errors
/// 404 for an item the tenant does not hold.
pub(super) async fn get(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
) -> Result<(super::dto::PricingPlanItemReadDto, u64), DoorError> {
    let m = plan_item_repo::find(tx, scope, tenant, id)
        .await?
        .ok_or_else(|| support::missing_what("plan_item"))?;
    let children = AccessScope::for_tenant(tenant);
    let r = plans::find_revision(tx, &children, tenant, m.revision_id).await?;
    let siblings = plan_revision_repo::for_plan(tx, &children, tenant, r.plan_id).await?;
    let state = crate::infra::plan_revisions::effective_revisions(&siblings, plans::today())?
        .into_iter()
        .find(|e| e.id == r.id)
        .map_or_else(
            || PricingRevisionState::stored(&r.state, &format_args!("revision {} state", r.id)),
            |e| Ok(e.state.into()),
        )?;
    let version = crate::api::rest::preconditions::RowVersion::from_stored(m.version)
        .map_err(CanonicalError::from)?
        .get();
    Ok((
        super::dto::PricingPlanItemReadDto {
            item: m.try_into()?,
            plan_id: r.plan_id,
            rev_no: r.rev_no,
            state,
        },
        version,
    ))
}
enum Begun {
    Replay(Receipt),
    Op(Uuid),
}
/// Tx A of an item create, then the drive (D-401): claim the key, mint the item id and write the
/// create op before any reserve; the drive reserves, re-reads the SKU, writes the item (Tx B,
/// which re-reads the revision and the entry) and confirms. A 503 writes nothing.
///
/// A replay or an in-flight duplicate is answered from the key's store alone. Tx A refuses a
/// revision that is missing (404) or no longer an unlocked draft (409 `REVISION_NOT_DRAFT`).
/// # Errors
/// The canonical refusal, conflict or unavailability of any step.
#[expect(
    clippy::too_many_arguments,
    reason = "authorized door identity and replay operands"
)]
pub async fn create(
    state: Arc<AuthoringState>,
    scope: AccessScope,
    ctx: SecurityContext,
    revision: Uuid,
    correlation: Uuid,
    key: String,
    digest: Vec<u8>,
    input: PricingPlanItemCreate,
) -> Result<Response, CanonicalError> {
    let original_ctx = ctx.clone();
    let endpoint = format!("/bss-pricing/v1/plan-revisions/{revision}/items");
    if let Some(receipt) = stored(&state, ctx.subject_tenant_id(), &endpoint, &key, &digest).await?
    {
        return receipt.response();
    }
    let result = support::transaction(&state.db.db(), move |tx| {
        let (scope, ctx, key, digest, input, endpoint) = (
            scope.clone(),
            ctx.clone(),
            key.clone(),
            digest.clone(),
            input.clone(),
            endpoint.clone(),
        );
        Box::pin(async move {
            let tenant = ctx.subject_tenant_id();
            let now = crate::infra::storage::stored_now();
            let receipt_scope = AccessScope::for_tenant(tenant);
            let claim = idem::claim_idempotency_key(
                tx,
                &receipt_scope,
                tenant,
                &endpoint,
                &key,
                &digest,
                now,
                now + time::Duration::hours(24),
            )
            .await?;
            if let Some(receipt) = settled(claim, &digest)? {
                return Ok(Begun::Replay(receipt));
            }
            let Some(parent) = plan_revision_repo::find(tx, &scope, tenant, revision).await? else {
                return Err(support::missing_what("plan_revision").into());
            };
            if parent.state != RevisionState::Draft.as_str() || parent.pending_unit_id.is_some() {
                return Err(support::conflict("REVISION_NOT_DRAFT").into());
            }
            let reference = Ref {
                kind: RefKind::PlanItem,
                id: Uuid::now_v7(),
                sku_id: input.sku_id,
            };
            let work = Work {
                target: Target::PlanItem {
                    revision_id: revision,
                    input: Some(input.into()),
                },
                correlation,
                refusal: None,
                receipt: None,
                outcome: None,
                reason: None,
            };
            let op = reference_work::new_op(
                &ctx,
                reference,
                &work,
                OpKind::Create,
                None,
                Some(key.clone()),
                now,
            )?;
            let id = op.op_id;
            reference_op_repo::insert(tx, &receipt_scope, op).await?;
            idem::bind_op(tx, &receipt_scope, tenant, &endpoint, &key, id).await?;
            Ok(Begun::Op(id))
        })
    })
    .await?;
    match result {
        Begun::Replay(receipt) => receipt.response(),
        Begun::Op(id) => {
            reference_work::drive(&state, &original_ctx, id, Arc::new(WallClock), Caller::Door)
                .await?
                .ok_or_else(|| CanonicalError::internal("missing create receipt").create())?
                .response()
        }
    }
}
/// Remove one item of an unlocked draft revision of the caller and write its delete op, in the
/// caller's transaction; the caller drives the op once it commits. An item whose confirm is still
/// pending is kept until it completes (`ITEM_CONFIRMATION_PENDING`), or its confirm could lose
/// it; an item of a pending, published or superseded revision is never removed
/// (`REVISION_NOT_DRAFT`): its reference outlives the revision (D-414); an item of another
/// author's draft is not the caller's to remove (`NOT_DRAFT_AUTHOR`, D-404). A draft revision's
/// delete removes every item through this, before the revision itself.
/// # Errors
/// 404 for an unknown item, the refusals above, `STALE_REVISION` for a lost race.
pub(crate) async fn remove(
    tx: &impl DBRunner,
    scope: &AccessScope,
    ctx: &SecurityContext,
    correlation: Uuid,
    id: Uuid,
) -> Result<Uuid, DoorError> {
    let tenant = ctx.subject_tenant_id();
    let item = plan_item_repo::find(tx, scope, tenant, id)
        .await?
        .ok_or_else(|| support::missing_what("plan_item"))?;
    let revision = plans::find_revision(tx, scope, tenant, item.revision_id).await?;
    plans::editable(&revision, ctx)?;
    if item.reference_state == ReferenceState::ConfirmationPending.as_str() {
        return Err(support::conflict("ITEM_CONFIRMATION_PENDING").into());
    }
    let now = crate::infra::storage::stored_now();
    let op = reference_work::plan_item::delete_op(ctx, &item, correlation, now)?;
    let op_id = op.op_id;
    plan_item_repo::delete_draft(tx, scope, tenant, id, item.version).await?;
    reference_op_repo::insert(tx, &AccessScope::for_tenant(tenant), op).await?;
    support::audit(tx, ctx, correlation, "plan_item.delete", id, item.version).await?;
    Ok(op_id)
}
/// `DELETE /plan-items/{id}` below its door: the removal and its delete op commit together
/// (204); the release is durable work, and what this call does not finish, the ticker does.
/// # Errors
/// The removal's refusals; a failed release is never an error of the delete.
pub async fn delete(
    state: Arc<AuthoringState>,
    scope: AccessScope,
    ctx: SecurityContext,
    correlation: Uuid,
    id: Uuid,
) -> Result<Response, CanonicalError> {
    let original_ctx = ctx.clone();
    let op_id = support::transaction(&state.db.db(), move |tx| {
        let (scope, ctx) = (scope.clone(), ctx.clone());
        Box::pin(async move { remove(tx, &scope, &ctx, correlation, id).await })
    })
    .await?;
    drive_best_effort(&state, &original_ctx, &[op_id]).await;
    Ok(StatusCode::NO_CONTENT.into_response())
}
/// Drive committed ops once each under the caller, stopping at the first drive that fails (any
/// error): a registry that is slow or down is met once per request, not once per op. Every op
/// this does not finish stays durable, and the ticker finishes it after the in-flight grace
/// (D-413).
pub async fn drive_best_effort(state: &Arc<AuthoringState>, ctx: &SecurityContext, ops: &[Uuid]) {
    for (done, op_id) in ops.iter().enumerate() {
        if let Err(error) =
            reference_work::drive(state, ctx, *op_id, Arc::new(WallClock), Caller::Door).await
        {
            tracing::warn!(
                op_id=%op_id,
                error=%error,
                diagnostic=error.diagnostic().unwrap_or_default(),
                left = ops.len() - done,
                "pricing plan item reference work deferred to the ticker"
            );
            break;
        }
    }
}
