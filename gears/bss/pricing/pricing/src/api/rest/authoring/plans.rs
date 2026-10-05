//! Plans and their revisions below their doors (D-404, D-407, D-413, D-414): a plan with its
//! draft rev 1, the rename, the copy of the published revision into a new draft, the clone of a
//! plan's published revision into a new plan, the draft revision's PATCH and delete, and the
//! revision's checks read over fresh SKUs (D-408).
//!
//! A copy writes the revision, every copied item (`unreserved`, no receipt) and one attach op per
//! item in ONE transaction (D-413); the door then drives the attach ops best-effort and answers
//! 201, and the ticker finishes what it could not. A draft revision belongs to its author (D-404):
//! only its `created_by` edits or deletes it and its items.
//!
//! Every read renders a revision's state as it reads today (D-447), derived in memory from the
//! stored rows; no read writes. The three doors that can meet a due scheduled revision — the
//! copy, the clone and the unschedule door — persist its switch first (D-451), with the job's
//! event and audit row.
//!
//! @cpt-dod:cpt-cf-bss-pricing-dod-plan-clone:p1
use super::{
    AuthoringState, configuration,
    dto::{
        self, PlanReading, PricingPlanApprovalProgress, PricingPlanChecksDto, PricingPlanClone,
        PricingPlanCreate, PricingPlanDto, PricingPlanList, PricingPlanPatch,
        PricingPlanRevisionDto, PricingPlanRevisionPatch,
    },
    plan_items,
    support::{self, DoorError},
};
use crate::{
    domain::{
        book::Book,
        plan::{self, PlanContext, ReferenceState, RevisionState},
        price::PriceState,
    },
    infra::{
        events::TxOutbox,
        plan_revisions, reference_registry, reference_work,
        storage::{
            RepoError,
            entity::{plan as plan_entity, plan_item, plan_revision, price, price_book_entry},
            repo::{
                approval_repo, book_repo, dimension_repo, plan_item_repo, plan_repo,
                plan_revision_repo, price_book_entry_repo, price_repo, reference_op_repo,
            },
        },
    },
};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use bss_products_sdk::models::Sku;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::secure::{AccessScope, DBRunner};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// A plan of the caller's tenant, or 404.
pub(super) async fn find_plan(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
) -> Result<plan_entity::Model, DoorError> {
    plan_repo::find(tx, scope, tenant, id)
        .await?
        .ok_or_else(|| support::missing_what("plan").into())
}
/// A revision of the caller's tenant, or 404.
pub(super) async fn find_revision(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
) -> Result<plan_revision::Model, DoorError> {
    plan_revision_repo::find(tx, scope, tenant, id)
        .await?
        .ok_or_else(|| support::missing_what("plan_revision").into())
}
/// Whether the revision is a draft no pending unit holds.
pub(super) fn open_draft(r: &plan_revision::Model) -> bool {
    r.state == RevisionState::Draft.as_str() && r.pending_unit_id.is_none()
}
/// A revision the caller may edit, with its items: an unlocked draft (else 409
/// `REVISION_NOT_DRAFT`) that the caller created (else 403 `NOT_DRAFT_AUTHOR`, D-404).
/// # Errors
/// The two refusals above.
pub(super) fn editable(r: &plan_revision::Model, ctx: &SecurityContext) -> Result<(), DoorError> {
    if !open_draft(r) {
        return Err(support::conflict("REVISION_NOT_DRAFT").into());
    }
    if r.created_by != ctx.subject_id() {
        return Err(support::forbidden_because(
            "NOT_DRAFT_AUTHOR",
            format!("plan revision {} is a draft of another author", r.id),
        )
        .into());
    }
    Ok(())
}
/// Today, the UTC date of now: the day every read derives a revision's state on (D-447), as the
/// checks judge a sale date on it.
pub(super) fn today() -> time::Date {
    time::OffsetDateTime::now_utc().date()
}
/// What the plan DTO shows beside the rows of the plans whose revisions `revisions` yields (one
/// slice per plan): the item SKUs of each plan's current revision and of the revision in effect
/// (D-460, D-480) and the instants of every unit the revisions name (D-461), ONE grouped statement
/// each whatever the number of plans. It borrows the revisions: a single plan's read clones none
/// (the phase 9 review's R51).
async fn plan_reading<'a>(
    tx: &impl DBRunner,
    tenant: Uuid,
    revisions: impl IntoIterator<Item = &'a [plan_revision::Model]>,
    today: time::Date,
) -> Result<PlanReading, DoorError> {
    let children = AccessScope::for_tenant(tenant);
    let (mut wanted, mut units, mut book_ids) = (Vec::new(), Vec::new(), Vec::new());
    for own in revisions {
        // Every header names its book (D-516). The same grouped read still covers `current`.
        book_ids.extend(own.iter().map(|r| r.book_id));
        wanted.extend(dto::current_revision(own, today)?);
        wanted.extend(dto::in_effect_revision(own, today)?);
        units.extend(dto::named_units(own));
    }
    wanted.sort_unstable();
    wanted.dedup();
    book_ids.sort_unstable();
    book_ids.dedup();
    let books = book_repo::find_many(tx, &children, tenant, &book_ids)
        .await?
        .into_iter()
        .map(|book| (book.id, dto::PricingPlanBook::of(&book)))
        .collect();
    Ok(PlanReading {
        skus: plan_item_repo::skus_of_revisions(tx, &children, tenant, &wanted).await?,
        units: approval_repo::unit_instants(tx, &children, tenant, &units).await?,
        books,
    })
}
async fn plan_body(
    tx: &impl DBRunner,
    tenant: Uuid,
    m: plan_entity::Model,
) -> Result<PricingPlanDto, DoorError> {
    let own =
        plan_revision_repo::for_plan(tx, &AccessScope::for_tenant(tenant), tenant, m.id).await?;
    let today = today();
    let reading = plan_reading(tx, tenant, [own.as_slice()], today).await?;
    Ok(PricingPlanDto::of(m, &own, today, &reading)?)
}
/// A revision read (D-480): its items, its state among its plan's revisions as it reads today,
/// the instants of the unit it names (D-461), its vote progress while pending (D-462), then one
/// grouped read of the entries the items name, one admission of those entries' books under the
/// caller's `price_book` read (`books`, or none without that grant) and one grouped read of the
/// admitted entries' default-chain prices on the sale date. A draft or pending revision also
/// reads the in-effect revision's item SKUs, one statement whether or not one is in effect.
async fn revision_read(
    tx: &impl DBRunner,
    books: Option<&AccessScope>,
    tenant: Uuid,
    m: plan_revision::Model,
) -> Result<dto::PricingPlanRevisionReadDto, DoorError> {
    let children = AccessScope::for_tenant(tenant);
    let items = plan_item_repo::for_revision(tx, &children, tenant, m.id).await?;
    let mut entry_ids: Vec<Uuid> = items.iter().filter_map(|i| i.price_book_entry_id).collect();
    entry_ids.sort_unstable();
    entry_ids.dedup();
    let siblings = plan_revision_repo::for_plan(tx, &children, tenant, m.plan_id).await?;
    let today = today();
    let mut dto = PricingPlanRevisionDto::read(&m, &siblings, items, today)?;
    if let Some(named) = m.pending_unit_id.or(m.approved_by_unit_id)
        && let Some(unit) = approval_repo::find_unit(tx, &children, tenant, named)
            .await
            .map_err(support::approval_failure)?
    {
        let approval = progress(tx, tenant, &unit).await?;
        dto = dto.with_units(&instants_of(&unit), approval);
    }
    let sale = plan::sale_date(
        &plan::Revision {
            id: m.id,
            rev_no: m.rev_no,
            book_id: m.book_id,
            state: dto.state.into(),
            available_from: m.available_from,
        },
        today,
    );
    let entries = price_book_entry_repo::find_many(tx, &children, tenant, &entry_ids).await?;
    let mut book_ids: Vec<Uuid> = entries.iter().map(|e| e.book_id).collect();
    book_ids.sort_unstable();
    book_ids.dedup();
    let admitted: BTreeSet<Uuid> = match books {
        Some(scope) => book_repo::find_many(tx, scope, tenant, &book_ids)
            .await?
            .into_iter()
            .map(|b| b.id)
            .collect(),
        None => BTreeSet::new(),
    };
    let shown: Vec<&price_book_entry::Model> = entries
        .iter()
        .filter(|e| admitted.contains(&e.book_id))
        .collect();
    let mut headlines = if shown.is_empty() {
        BTreeMap::new()
    } else {
        super::price_book_entries::headline(tx, tenant, &shown, sale).await?
    };
    let summaries = entries
        .into_iter()
        .map(|e| {
            let price = admitted
                .contains(&e.book_id)
                .then(|| headlines.remove(&e.id).and_then(|h| h.current))
                .flatten();
            Ok(dto::PricingPlanEntrySummary::of(e, price)?)
        })
        .collect::<Result<Vec<_>, DoorError>>()?;
    let state: plan::RevisionState = dto.state.into();
    let carried_sku_ids = if matches!(
        state,
        plan::RevisionState::Draft | plan::RevisionState::Pending
    ) {
        let id = dto::in_effect_revision(&siblings, today)?;
        let ids: Vec<Uuid> = id.into_iter().collect();
        let map = plan_item_repo::skus_of_revisions(tx, &children, tenant, &ids).await?;
        Some(id.and_then(|i| map.get(&i).cloned()).unwrap_or_default())
    } else {
        None
    };
    Ok(dto::PricingPlanRevisionReadDto {
        revision: dto,
        sale_date: sale.to_string(),
        entries: summaries,
        carried_sku_ids,
    })
}
/// A unit's instants, keyed as the DTOs read them (D-461).
pub(super) fn instants_of(
    unit: &bss_approval::Unit,
) -> BTreeMap<Uuid, approval_repo::UnitInstants> {
    BTreeMap::from([(
        unit.id,
        approval_repo::UnitInstants {
            id: unit.id,
            submitted_at: unit.submitted_at,
            decided_at: unit.decided_at,
        },
    )])
}
/// A pending unit's vote progress (D-462, O-9a): the approve votes the quorum counts, by the
/// approval library's `counted_approvals` over the unit's decisions (the count the vote door
/// judges by: it reads no item and names no actor), and the quorum; `None` for a unit that is not
/// pending. One statement, read with the tenant's scope under the revision read's plan read:
/// counts only.
pub(super) async fn progress(
    tx: &impl DBRunner,
    tenant: Uuid,
    unit: &bss_approval::Unit,
) -> Result<Option<PricingPlanApprovalProgress>, DoorError> {
    if unit.state != bss_approval::UnitState::Pending {
        return Ok(None);
    }
    let decisions =
        approval_repo::decisions_of_units(tx, &AccessScope::for_tenant(tenant), tenant, &[unit.id])
            .await?
            .remove(&unit.id)
            .unwrap_or_default();
    Ok(progress_of(unit, &decisions))
}
/// [`progress`] over a unit's decisions already read.
pub(super) fn progress_of(
    unit: &bss_approval::Unit,
    decisions: &[bss_approval::Decision],
) -> Option<PricingPlanApprovalProgress> {
    (unit.state == bss_approval::UnitState::Pending).then(|| PricingPlanApprovalProgress {
        unit_id: unit.id,
        approvals: bss_approval::counted_approvals(unit, decisions),
        quorum_required: unit.quorum_required,
    })
}
fn etag(version: i64) -> Result<u64, CanonicalError> {
    Ok(
        crate::api::rest::preconditions::RowVersion::from_stored(version)
            .map_err(CanonicalError::from)?
            .get(),
    )
}

/// The book a plan names is one its author may read (D-456, extending D-440's money rule):
/// `books` is the caller's `price_book` read, `None` without that grant. A book of the tenant it
/// does not admit is 403 `PRICE_BOOK_READ_REQUIRED`.
/// # Errors
/// That refusal; storage failures.
async fn require_book_read(
    tx: &impl DBRunner,
    books: Option<&AccessScope>,
    tenant: Uuid,
    book: Uuid,
) -> Result<(), DoorError> {
    if super::price_book_entries::shows_money(tx, books, tenant, book).await? {
        Ok(())
    } else {
        Err(support::forbidden_because(
            "PRICE_BOOK_READ_REQUIRED",
            "a plan names a book: authoring it takes price_book read on that book",
        )
        .into())
    }
}
/// A new plan's code (D-468): blank is 400 `PLAN_CODE_REQUIRED`, as before; then a code that does
/// not follow the rule ([`plan::code_follows_the_rule`]) is 400 `PLAN_CODE_INVALID`, judged as
/// sent. The door's length cap (`FIELD_TOO_LONG`, 64, D-457) is judged before this, with the body.
fn judge_code(code: &str) -> Result<(), DoorError> {
    if code.trim().is_empty() {
        return Err(support::invalid("code", "PLAN_CODE_REQUIRED").into());
    }
    if !plan::code_follows_the_rule(code) {
        return Err(support::invalid_because(
            "code",
            "PLAN_CODE_INVALID",
            "a plan code is 1 to 32 characters of A-Z, 0-9, - and _, starting with a letter or \
             a digit",
        )
        .into());
    }
    Ok(())
}
/// `POST /plans`: the plan and its draft rev 1 on the named book, with the body's sale date if it
/// names one (D-463), in the key's transaction. The book is one the caller's `price_book` read
/// admits (`books`, D-456).
/// # Errors
/// 400 `PLAN_CODE_REQUIRED`, `PLAN_CODE_INVALID` (D-468) or `DATE_INVALID`; 404 for a book the tenant does not hold; 403
/// `PRICE_BOOK_READ_REQUIRED` for one the caller may not read; 409 `PLAN_CODE_TAKEN`; a replayed
/// or conflicting key.
pub(super) async fn create(
    tx: &impl DBRunner,
    (scope, books): (&AccessScope, Option<&AccessScope>),
    ctx: &SecurityContext,
    correlation: Uuid,
    (key, digest): (&str, &[u8]),
    input: PricingPlanCreate,
) -> Result<Response, DoorError> {
    let tenant = ctx.subject_tenant_id();
    let endpoint = "/bss-pricing/v1/plans";
    if let Some(replay) = support::claim(tx, tenant, endpoint, key, digest).await? {
        return Ok(replay);
    }
    judge_code(&input.code)?;
    // D-463: judged as the revision PATCH judges it, among the body's refusals (D-456's order).
    let available_from = support::date(input.available_from.clone(), "available_from")?;
    let children = AccessScope::for_tenant(tenant);
    let Some(book) = book_repo::find(tx, &children, tenant, input.book_id).await? else {
        return Err(support::missing().into());
    };
    require_book_read(tx, books, tenant, input.book_id).await?;
    let now = crate::infra::storage::stored_now();
    // @cpt-begin:cpt-cf-bss-pricing-flow-plans:p1:inst-plans-flow-1
    let p = plan_repo::insert(
        tx,
        scope,
        plan_entity::Model {
            id: Uuid::now_v7(),
            tenant_id: tenant,
            code: input.code,
            name: input.name,
            published_rev: None,
            version: 1,
            created_by: ctx.subject_id(),
            created_at: now,
            updated_at: now,
            work_revision_id: None,
            work_state: None,
            scheduled_revision_id: None,
            scheduled_from: None,
            published_revision_id: None,
            current_book_id: None,
            current_currency: None,
            last_activity_at: now,
        },
    )
    .await?;
    let r = plan_revision_repo::insert(
        tx,
        &children,
        plan_revision::Model {
            id: Uuid::now_v7(),
            tenant_id: tenant,
            plan_id: p.id,
            rev_no: 1,
            book_id: input.book_id,
            state: RevisionState::Draft.as_str().into(),
            available_from,
            pending_unit_id: None,
            approved_by_unit_id: None,
            published_at: None,
            version: 1,
            created_by: ctx.subject_id(),
            created_at: now,
            updated_at: now,
        },
    )
    .await?;
    // @cpt-end:cpt-cf-bss-pricing-flow-plans:p1:inst-plans-flow-1
    support::audit(tx, ctx, correlation, "plan.create", p.id, 1).await?;
    support::audit(tx, ctx, correlation, "plan_revision.create", r.id, 1).await?;
    // A write answers what it wrote (D-453): an empty draft that names no unit (D-460, D-461),
    // on the book the body named.
    let reading = PlanReading {
        skus: BTreeMap::new(),
        units: BTreeMap::new(),
        books: BTreeMap::from([(book.id, dto::PricingPlanBook::of(&book))]),
    };
    let body = PricingPlanDto::of(p, &[r], today(), &reading)?;
    support::answer(
        tx,
        tenant,
        endpoint,
        key,
        StatusCode::CREATED,
        &body,
        Some(1),
    )
    .await
}
/// `GET /plans` (D-485): one page of the tenant's plans. The page query carries the narrowing,
/// including `sku_id`'s stored-state `EXISTS` (D-434). Then four grouped reads: revision headers,
/// the current and in-effect items, the units, and the revisions' books (D-516). Five statements
/// for a non-empty page, whatever its size.
/// # Errors
/// 400 for a query the pager refuses; storage failures.
pub(super) async fn list(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    backend: sea_orm::DbBackend,
    filter: &plan_repo::PlanListFilter,
    query: &toolkit_odata::ODataQuery,
) -> Result<PricingPlanList, DoorError> {
    let page = plan_repo::page(tx, scope, tenant, backend, filter, query)
        .await
        .map_err(list_failure)?;
    if page.items.is_empty() {
        return Ok(PricingPlanList {
            items: Vec::new(),
            page_info: page.page_info,
        });
    }
    let ids: Vec<Uuid> = page.items.iter().map(|p| p.id).collect();
    let mut revisions: BTreeMap<Uuid, Vec<plan_revision::Model>> = BTreeMap::new();
    for r in
        plan_revision_repo::for_plans(tx, &AccessScope::for_tenant(tenant), tenant, &ids).await?
    {
        revisions.entry(r.plan_id).or_default().push(r);
    }
    let reading = plan_reading(
        tx,
        tenant,
        revisions.values().map(Vec::as_slice),
        filter.today,
    )
    .await?;
    Ok(PricingPlanList {
        items: page
            .items
            .into_iter()
            .map(|p| {
                let own = revisions.remove(&p.id).unwrap_or_default();
                PricingPlanDto::of(p, &own, filter.today, &reading)
            })
            .collect::<Result<_, _>>()?,
        page_info: page.page_info,
    })
}
/// `GET /plans/counts` (D-485): the list's narrowing, one grouped statement.
/// # Errors
/// The list's query refusals; storage failures.
pub(super) async fn counts(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    backend: sea_orm::DbBackend,
    filter: &plan_repo::PlanListFilter,
    query: &toolkit_odata::ODataQuery,
) -> Result<dto::PricingPlanCounts, DoorError> {
    let counts = plan_repo::count(tx, scope, tenant, backend, filter, query)
        .await
        .map_err(list_failure)?;
    Ok(dto::PricingPlanCounts {
        by_selling: dto::PricingPlanSellingCounts {
            r#true: counts.selling_true,
            r#false: counts.selling_false,
        },
        by_change: dto::PricingPlanChangeCounts {
            none: counts.none,
            draft: counts.draft,
            pending: counts.pending,
            scheduled: counts.scheduled,
        },
        total: counts.total,
    })
}
fn list_failure(error: plan_repo::PlanListError) -> DoorError {
    match error {
        plan_repo::PlanListError::Query(error) => DoorError::Api(error.into()),
        plan_repo::PlanListError::Repo(error) => DoorError::Repo(error),
    }
}
/// `GET /plans/{id}`: the plan and its version. The door names its actors (D-519).
/// # Errors
/// 404 for a plan the tenant does not hold.
pub(super) async fn get(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
) -> Result<(PricingPlanDto, u64), DoorError> {
    let m = find_plan(tx, scope, tenant, id).await?;
    let version = etag(m.version)?;
    Ok((plan_body(tx, tenant, m).await?, version))
}
/// `PATCH /plans/{id}`: rename at the version the caller read.
/// # Errors
/// 404; 409 `STALE_REVISION`.
pub(super) async fn patch(
    tx: &impl DBRunner,
    scope: &AccessScope,
    ctx: &SecurityContext,
    correlation: Uuid,
    id: Uuid,
    version: u64,
    input: PricingPlanPatch,
) -> Result<Response, DoorError> {
    let tenant = ctx.subject_tenant_id();
    let m = find_plan(tx, scope, tenant, id).await?;
    support::check_version(version, m.version)?;
    let now = crate::infra::storage::stored_now();
    plan_repo::rename(tx, scope, tenant, id, m.version, input.name.clone(), now).await?;
    let m = plan_entity::Model {
        name: input.name,
        version: m.version + 1,
        updated_at: now,
        ..m
    };
    support::audit(tx, ctx, correlation, "plan.patch", id, m.version).await?;
    Ok(support::response(
        StatusCode::OK,
        &plan_body(tx, tenant, m).await?,
        Some(version + 1),
    )?)
}

/// `POST /plans/{id}/revisions`: copy the published revision into a new draft (D-413), then drive
/// the attach ops of its items best-effort and answer 201 with the answer the key recorded. A due
/// scheduled revision is switched first (D-451), so the copy is of the revision in effect.
/// # Errors
/// 404 for an unknown plan; 409 `REVISION_DRAFT_EXISTS` while a draft or pending revision exists;
/// 409 `REVISION_SCHEDULED` while a revision waits for its sale date (D-451); 409
/// `PLAN_UNPUBLISHED` when there is no published revision to copy; a replayed or conflicting key.
pub(super) async fn copy(
    state: Arc<AuthoringState>,
    scope: AccessScope,
    ctx: SecurityContext,
    correlation: Uuid,
    plan_id: Uuid,
    key: String,
    digest: Vec<u8>,
) -> Result<Response, CanonicalError> {
    let original_ctx = ctx.clone();
    let db = state.db.db();
    let (response, ops) =
        support::transaction_with_events(&db, &state.outbox, move |tx, outbox| {
            let (scope, ctx, key, digest) =
                (scope.clone(), ctx.clone(), key.clone(), digest.clone());
            Box::pin(async move {
                copy_in(
                    tx,
                    &outbox,
                    &scope,
                    &ctx,
                    correlation,
                    plan_id,
                    (&key, &digest),
                )
                .await
            })
        })
        .await?;
    plan_items::drive_best_effort(&state, &original_ctx, &ops).await;
    Ok(response)
}
async fn copy_in(
    tx: &(impl DBRunner + Sync),
    outbox: &TxOutbox,
    scope: &AccessScope,
    ctx: &SecurityContext,
    correlation: Uuid,
    plan_id: Uuid,
    (key, digest): (&str, &[u8]),
) -> Result<(Response, Vec<Uuid>), DoorError> {
    let tenant = ctx.subject_tenant_id();
    let endpoint = format!("/bss-pricing/v1/plans/{plan_id}/revisions");
    if let Some(replay) = support::claim(tx, tenant, &endpoint, key, digest).await? {
        return Ok((replay, Vec::new()));
    }
    let children = AccessScope::for_tenant(tenant);
    let p = find_plan(tx, scope, tenant, plan_id).await?;
    let now = crate::infra::storage::stored_now();
    // @cpt-begin:cpt-cf-bss-pricing-algo-plans-revision-switch:p1:inst-plans-revision-switch-3
    plan_revisions::catch_up(tx, outbox, tenant, p.id, now, correlation).await?;
    let revisions = plan_revision_repo::for_plan(tx, &children, tenant, p.id).await?;
    let open = [
        RevisionState::Draft.as_str(),
        RevisionState::Pending.as_str(),
    ];
    if revisions.iter().any(|r| open.contains(&r.state.as_str())) {
        return Err(support::conflict("REVISION_DRAFT_EXISTS").into());
    }
    // D-451: one scheduled revision at a time, and nothing after it: withdraw it or wait.
    if revisions
        .iter()
        .any(|r| r.state == RevisionState::Scheduled.as_str())
    {
        return Err(support::conflict("REVISION_SCHEDULED").into());
    }
    // @cpt-end:cpt-cf-bss-pricing-algo-plans-revision-switch:p1:inst-plans-revision-switch-3
    // @cpt-begin:cpt-cf-bss-pricing-flow-plans:p1:inst-plans-flow-1
    let source = revisions
        .iter()
        .find(|r| r.state == RevisionState::Published.as_str())
        .ok_or_else(|| support::conflict("PLAN_UNPUBLISHED"))?;
    let rev_no = revisions
        .iter()
        .map(|r| r.rev_no)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| support::conflict("REVISION_NO_TAKEN"))?;
    let r = plan_revision_repo::insert(
        tx,
        &children,
        plan_revision::Model {
            id: Uuid::now_v7(),
            tenant_id: tenant,
            plan_id,
            rev_no,
            book_id: source.book_id,
            state: RevisionState::Draft.as_str().into(),
            available_from: source.available_from,
            pending_unit_id: None,
            approved_by_unit_id: None,
            published_at: None,
            version: 1,
            created_by: ctx.subject_id(),
            created_at: now,
            updated_at: now,
        },
    )
    .await?;
    let (items, ops) = copy_items(tx, &children, ctx, correlation, source.id, r.id, now).await?;
    // @cpt-end:cpt-cf-bss-pricing-flow-plans:p1:inst-plans-flow-1
    support::audit(tx, ctx, correlation, "plan_revision.copy", r.id, 1).await?;
    let body = PricingPlanRevisionDto::of(&r, items)?;
    let response = support::answer(
        tx,
        tenant,
        &endpoint,
        key,
        StatusCode::CREATED,
        &body,
        Some(1),
    )
    .await?;
    Ok((response, ops))
}

/// Copy every item of the `source` revision into the new draft `target`, in the caller's
/// transaction (D-413): each copy is written `unreserved` with no receipt and authored by the
/// caller, with one attach op per copy. Answers the copies and their op ids, for the door to drive
/// after its commit.
async fn copy_items(
    tx: &impl DBRunner,
    children: &AccessScope,
    ctx: &SecurityContext,
    correlation: Uuid,
    source: Uuid,
    target: Uuid,
    now: time::OffsetDateTime,
) -> Result<(Vec<plan_item::Model>, Vec<Uuid>), DoorError> {
    let tenant = ctx.subject_tenant_id();
    let mut items = Vec::new();
    let mut ops = Vec::new();
    for from in plan_item_repo::for_revision(tx, children, tenant, source).await? {
        // D-467: a copy is a new row, which the repository writes `paid` with no quantity
        // whatever the source row carries; an item without an entry stays one (D-512), so the
        // draft's checks show it ITEM_ENTRY_MISSING.
        let copy = plan_item_repo::insert(
            tx,
            children,
            plan_item::Model {
                id: Uuid::now_v7(),
                revision_id: target,
                reservation_id: None,
                reference_state: ReferenceState::Unreserved.as_str().into(),
                version: 1,
                created_by: ctx.subject_id(),
                created_at: now,
                updated_at: now,
                ..from
            },
        )
        .await?;
        // @cpt-begin:cpt-cf-bss-pricing-flow-plans:p1:inst-plans-flow-2
        let op = reference_work::attach_op(ctx, &copy, correlation, now)?;
        ops.push(op.op_id);
        reference_op_repo::insert(tx, children, op).await?;
        // @cpt-end:cpt-cf-bss-pricing-flow-plans:p1:inst-plans-flow-2
        items.push(copy);
    }
    Ok((items, ops))
}

/// `POST /plans/{id}/clone`: a new plan (its own code and name) whose draft rev 1 copies the
/// source plan's PUBLISHED revision — the one in effect: a due scheduled revision is switched
/// first (D-451) — book, sale date and items, under D-413, then drive the
/// attach ops of its items best-effort and answer 201 with the new plan. The body's
/// `available_from` overrides the copied sale date, and null clears it (D-463). Nothing of the
/// source's approval is copied: no decision, no `approved_by_unit_id` or `published_at`, no pin;
/// a deprecated SKU is carried, and the new plan's checks show it red (D-408).
/// # Errors
/// 400 `PLAN_CODE_REQUIRED`, `PLAN_CODE_INVALID` (D-468) or `DATE_INVALID`; 404 for a plan the
/// tenant does not hold; 409 `CLONE_SOURCE_UNPUBLISHED` when the source has no published revision; 403
/// `PRICE_BOOK_READ_REQUIRED` when the caller's `price_book` read (`books`) does not admit the
/// book the clone names, the source's (D-456); 409 `PLAN_CODE_TAKEN`; a replayed or conflicting
/// key.
#[expect(
    clippy::too_many_arguments,
    reason = "authorized context, replay identity and input belong to one transaction"
)]
pub(super) async fn clone(
    state: Arc<AuthoringState>,
    (scope, books): (AccessScope, Option<AccessScope>),
    ctx: SecurityContext,
    correlation: Uuid,
    source: Uuid,
    key: String,
    digest: Vec<u8>,
    input: PricingPlanClone,
) -> Result<Response, CanonicalError> {
    let original_ctx = ctx.clone();
    let db = state.db.db();
    let (response, ops) =
        support::transaction_with_events(&db, &state.outbox, move |tx, outbox| {
            let (scope, books, ctx, key, digest) = (
                scope.clone(),
                books.clone(),
                ctx.clone(),
                key.clone(),
                digest.clone(),
            );
            let input = input.clone();
            Box::pin(async move {
                clone_in(
                    tx,
                    &outbox,
                    (&scope, books.as_ref()),
                    &ctx,
                    correlation,
                    source,
                    (&key, &digest),
                    input,
                )
                .await
            })
        })
        .await?;
    plan_items::drive_best_effort(&state, &original_ctx, &ops).await;
    Ok(response)
}
#[expect(
    clippy::too_many_arguments,
    reason = "authorized context, replay identity and input belong to one transaction"
)]
async fn clone_in(
    tx: &(impl DBRunner + Sync),
    outbox: &TxOutbox,
    (scope, books): (&AccessScope, Option<&AccessScope>),
    ctx: &SecurityContext,
    correlation: Uuid,
    source: Uuid,
    (key, digest): (&str, &[u8]),
    input: PricingPlanClone,
) -> Result<(Response, Vec<Uuid>), DoorError> {
    let tenant = ctx.subject_tenant_id();
    let endpoint = format!("/bss-pricing/v1/plans/{source}/clone");
    if let Some(replay) = support::claim(tx, tenant, &endpoint, key, digest).await? {
        return Ok((replay, Vec::new()));
    }
    judge_code(&input.code)?;
    // D-463: omitted keeps the source's sale date; a date overrides it; null clears it. Judged as
    // the revision PATCH judges it, among the body's refusals (D-456's order).
    let available_from = input
        .available_from
        .clone()
        .map(|from| support::date(from, "available_from"))
        .transpose()?;
    let children = AccessScope::for_tenant(tenant);
    let from = find_plan(tx, scope, tenant, source).await?;
    let now = crate::infra::storage::stored_now();
    // @cpt-begin:cpt-cf-bss-pricing-algo-plans-revision-switch:p1:inst-plans-revision-switch-3
    plan_revisions::catch_up(tx, outbox, tenant, from.id, now, correlation).await?;
    // @cpt-end:cpt-cf-bss-pricing-algo-plans-revision-switch:p1:inst-plans-revision-switch-3
    // @cpt-begin:cpt-cf-bss-pricing-algo-plans-clone-and-retire:p1:inst-plans-clone-and-retire-1
    let published = plan_revision_repo::for_plan(tx, &children, tenant, from.id)
        .await?
        .into_iter()
        .find(|r| r.state == RevisionState::Published.as_str())
        .ok_or_else(|| support::conflict("CLONE_SOURCE_UNPUBLISHED"))?;
    // The clone names the source's book: its author must be able to read it (D-456).
    require_book_read(tx, books, tenant, published.book_id).await?;
    let p = plan_repo::insert(
        tx,
        scope,
        plan_entity::Model {
            id: Uuid::now_v7(),
            tenant_id: tenant,
            code: input.code,
            name: input.name,
            published_rev: None,
            version: 1,
            created_by: ctx.subject_id(),
            created_at: now,
            updated_at: now,
            work_revision_id: None,
            work_state: None,
            scheduled_revision_id: None,
            scheduled_from: None,
            published_revision_id: None,
            current_book_id: None,
            current_currency: None,
            last_activity_at: now,
        },
    )
    .await?;
    let r = plan_revision_repo::insert(
        tx,
        &children,
        plan_revision::Model {
            id: Uuid::now_v7(),
            tenant_id: tenant,
            plan_id: p.id,
            rev_no: 1,
            book_id: published.book_id,
            state: RevisionState::Draft.as_str().into(),
            available_from: available_from.unwrap_or(published.available_from),
            pending_unit_id: None,
            approved_by_unit_id: None,
            published_at: None,
            version: 1,
            created_by: ctx.subject_id(),
            created_at: now,
            updated_at: now,
        },
    )
    .await?;
    let (items, ops) = copy_items(tx, &children, ctx, correlation, published.id, r.id, now).await?;
    // @cpt-end:cpt-cf-bss-pricing-algo-plans-clone-and-retire:p1:inst-plans-clone-and-retire-1
    support::audit(tx, ctx, correlation, "plan.clone", p.id, 1).await?;
    support::audit(tx, ctx, correlation, "plan_revision.create", r.id, 1).await?;
    // A write answers what it wrote (D-453): the new draft with the items it copied (D-460).
    let mut skus: Vec<Uuid> = items.iter().map(|i| i.sku_id).collect();
    skus.sort_unstable();
    let book = book_repo::find(tx, &children, tenant, r.book_id)
        .await?
        .ok_or_else(support::missing)?;
    let reading = PlanReading {
        skus: BTreeMap::from([(r.id, skus)]),
        units: BTreeMap::new(),
        books: BTreeMap::from([(book.id, dto::PricingPlanBook::of(&book))]),
    };
    let body = PricingPlanDto::of(p, &[r], today(), &reading)?;
    let response = support::answer(
        tx,
        tenant,
        &endpoint,
        key,
        StatusCode::CREATED,
        &body,
        Some(1),
    )
    .await?;
    Ok((response, ops))
}

/// `GET /plan-revisions/{id}`: the revision with its items and its version, the instants of the
/// unit it names (D-461), its vote progress while pending (D-462) and the read-only fields of
/// D-480 (sale date, entry summaries, carried SKUs). `books` is the caller's `price_book` read,
/// `None` without that grant: an entry of a book it does not admit has a null sale-date price.
/// The money's 503 is the handler's, before this read, so a missing revision is 404 only after
/// the policy can judge (D-440). The door names its actors (D-519).
/// # Errors
/// 404 for a revision the tenant does not hold.
pub(super) async fn get_revision(
    tx: &impl DBRunner,
    scope: &AccessScope,
    books: Option<&AccessScope>,
    tenant: Uuid,
    id: Uuid,
) -> Result<(dto::PricingPlanRevisionReadDto, u64), DoorError> {
    let m = find_revision(tx, scope, tenant, id).await?;
    let version = etag(m.version)?;
    Ok((revision_read(tx, books, tenant, m).await?, version))
}
/// `GET /plan-revisions/{id}/reservations` (D-480): each item's reference, under plan read. Two
/// statements: the revision's find (404 when the tenant does not hold it) and its items.
/// # Errors
/// 404 for a revision the tenant does not hold.
pub(super) async fn reservations(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
) -> Result<Response, DoorError> {
    let m = find_revision(tx, scope, tenant, id).await?;
    let items =
        plan_item_repo::for_revision(tx, &AccessScope::for_tenant(tenant), tenant, m.id).await?;
    let items = items
        .iter()
        .map(dto::PricingPlanReservationItemDto::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    let settled = items.iter().all(|item| {
        !matches!(
            item.reference_state,
            crate::api::rest::closed_sets::PricingItemReferenceState::Unreserved
                | crate::api::rest::closed_sets::PricingItemReferenceState::ConfirmationPending
        )
    });
    Ok(support::response(
        StatusCode::OK,
        &dto::PricingPlanReservationsDto { items, settled },
        None,
    )?)
}
/// `POST /plan-revisions/{id}/unschedule` (D-452): a scheduled revision whose sale date has not
/// come returns to an unlocked draft — `approved_by_unit_id` cleared, its items and their
/// references kept — and the door answers the draft, its version the `ETag` its PATCH takes. The
/// order is the authorization (the handler's `plan:submit`), the key's claim, the plan's
/// catch-up (D-451), then the state: a due revision has just been switched and is in effect. The
/// applied unit stays applied; no event. A refusal rolls the catch-up back with it; the job
/// persists that switch.
/// # Errors
/// 404 for a revision the tenant does not hold; 409 `REVISION_IN_EFFECT` for a published revision;
/// 409 `REVISION_NOT_SCHEDULED` for a draft, pending or superseded one; a replayed or conflicting
/// key.
pub(super) async fn unschedule(
    state: Arc<AuthoringState>,
    scope: AccessScope,
    ctx: SecurityContext,
    correlation: Uuid,
    id: Uuid,
    key: String,
    digest: Vec<u8>,
) -> Result<Response, CanonicalError> {
    let db = state.db.db();
    support::transaction_with_events(&db, &state.outbox, move |tx, outbox| {
        let (scope, ctx, key, digest) = (scope.clone(), ctx.clone(), key.clone(), digest.clone());
        Box::pin(async move {
            unschedule_in(tx, &outbox, &scope, &ctx, correlation, id, (&key, &digest)).await
        })
    })
    .await
}
async fn unschedule_in(
    tx: &(impl DBRunner + Sync),
    outbox: &TxOutbox,
    scope: &AccessScope,
    ctx: &SecurityContext,
    correlation: Uuid,
    id: Uuid,
    (key, digest): (&str, &[u8]),
) -> Result<Response, DoorError> {
    let tenant = ctx.subject_tenant_id();
    let endpoint = format!("/bss-pricing/v1/plan-revisions/{id}/unschedule");
    if let Some(replay) = support::claim(tx, tenant, &endpoint, key, digest).await? {
        return Ok(replay);
    }
    let children = AccessScope::for_tenant(tenant);
    let r = find_revision(tx, scope, tenant, id).await?;
    let now = crate::infra::storage::stored_now();
    // @cpt-begin:cpt-cf-bss-pricing-algo-plans-revision-switch:p1:inst-plans-revision-switch-3
    plan_revisions::catch_up(tx, outbox, tenant, r.plan_id, now, correlation).await?;
    // @cpt-end:cpt-cf-bss-pricing-algo-plans-revision-switch:p1:inst-plans-revision-switch-3
    // @cpt-begin:cpt-cf-bss-pricing-algo-plans-revision-switch:p1:inst-plans-revision-switch-4
    let r = find_revision(tx, &children, tenant, id).await?;
    if r.state == RevisionState::Published.as_str() {
        return Err(support::conflict("REVISION_IN_EFFECT").into());
    }
    if r.state != RevisionState::Scheduled.as_str() {
        return Err(support::conflict("REVISION_NOT_SCHEDULED").into());
    }
    plan_revision_repo::unschedule(tx, &children, tenant, id, now).await?;
    let draft = plan_revision::Model {
        state: RevisionState::Draft.as_str().into(),
        approved_by_unit_id: None,
        version: r.version + 1,
        updated_at: now,
        ..r
    };
    support::audit(
        tx,
        ctx,
        correlation,
        "plan_revision.unschedule",
        id,
        draft.version,
    )
    .await?;
    // @cpt-end:cpt-cf-bss-pricing-algo-plans-revision-switch:p1:inst-plans-revision-switch-4
    let items = plan_item_repo::for_revision(tx, &children, tenant, id).await?;
    let body = PricingPlanRevisionDto::of(&draft, items)?;
    support::answer(
        tx,
        tenant,
        &endpoint,
        key,
        StatusCode::OK,
        &body,
        Some(etag(draft.version)?),
    )
    .await
}
/// `PATCH /plan-revisions/{id}`: the book and the sale date of an unlocked draft of the caller,
/// at the version the caller read. A book change remaps every item whose entry has a twin in the
/// new book (the same SKU, charge kind, period, model and policy digest, plus equal dimension key,
/// D-502); an unmatched item keeps its old
/// entry, which the checks then show foreign (`ITEM_BOOK_FOREIGN`).
/// A named book is one the caller's `price_book` read admits (`books`, D-456).
/// # Errors
/// 404; 409 `REVISION_NOT_DRAFT`; 403 `NOT_DRAFT_AUTHOR`; 409 `STALE_REVISION`; 400
/// `DATE_INVALID`; 404 for a book the tenant does not hold; 403 `PRICE_BOOK_READ_REQUIRED` for
/// one the caller may not read.
pub(super) async fn patch_revision(
    tx: &impl DBRunner,
    (scope, books): (&AccessScope, Option<&AccessScope>),
    ctx: &SecurityContext,
    correlation: Uuid,
    id: Uuid,
    version: u64,
    input: PricingPlanRevisionPatch,
) -> Result<Response, DoorError> {
    let tenant = ctx.subject_tenant_id();
    let children = AccessScope::for_tenant(tenant);
    let m = find_revision(tx, scope, tenant, id).await?;
    editable(&m, ctx)?;
    support::check_version(version, m.version)?;
    let now = crate::infra::storage::stored_now();
    let mut next = m.clone();
    if let Some(from) = input.available_from {
        next.available_from = support::date(from, "available_from")?;
    }
    let mut items = plan_item_repo::for_revision(tx, &children, tenant, id).await?;
    if let Some(book) = input.book_id {
        if book_repo::find(tx, &children, tenant, book)
            .await?
            .is_none()
        {
            return Err(support::missing().into());
        }
        require_book_read(tx, books, tenant, book).await?;
        if book != m.book_id {
            remap(tx, &children, ctx, correlation, book, &mut items, now).await?;
        }
        next.book_id = book;
    }
    next.updated_at = now;
    plan_revision_repo::update_draft(tx, &children, next.clone()).await?;
    next.version += 1;
    support::audit(
        tx,
        ctx,
        correlation,
        "plan_revision.patch",
        id,
        next.version,
    )
    .await?;
    Ok(support::response(
        StatusCode::OK,
        &PricingPlanRevisionDto::of(&next, items)?,
        Some(version + 1),
    )?)
}
/// Point each item at the new book's entry of the same (SKU, charge kind, period, model, policy
/// digest) and an equal dimension key (D-427, D-502). Without that full match it keeps its entry,
/// so the checks show `ITEM_BOOK_FOREIGN` instead of changing the policy. A moved item is written
/// in the shape of D-467 (`paid`, no quantity), as the item PATCH writes it, so a legacy row stops
/// being one.
async fn remap(
    tx: &impl DBRunner,
    scope: &AccessScope,
    ctx: &SecurityContext,
    correlation: Uuid,
    book: Uuid,
    items: &mut [plan_item::Model],
    now: time::OffsetDateTime,
) -> Result<(), DoorError> {
    let tenant = ctx.subject_tenant_id();
    let twins = price_book_entry_repo::for_book(tx, scope, tenant, book).await?;
    for item in items.iter_mut() {
        let Some(entry) = item.price_book_entry_id else {
            continue;
        };
        let Some(old) = price_book_entry_repo::find(tx, scope, tenant, entry).await? else {
            continue;
        };
        let Some(twin) = twins.iter().find(|e| {
            e.sku_id == old.sku_id
                && e.charge_kind == old.charge_kind
                && e.period == old.period
                && e.model == old.model
                && e.usage_policy_digest == old.usage_policy_digest
                && e.dimension_key == old.dimension_key
        }) else {
            continue;
        };
        item.price_book_entry_id = Some(twin.id);
        item.updated_at = now;
        // The repository rewrites the row in D-467's shape (`plan_item_repo::update_draft`).
        plan_item_repo::update_draft(tx, scope, item.clone()).await?;
        item.version += 1;
        support::audit(
            tx,
            ctx,
            correlation,
            "plan_item.remap",
            item.id,
            item.version,
        )
        .await?;
    }
    Ok(())
}
/// `DELETE /plan-revisions/{id}`: remove an unlocked draft of the caller with every item, each
/// with its delete op, in one transaction (D-414); then drive the releases best-effort and
/// answer 204. The last revision of a never-published plan takes the plan with it in the same
/// transaction, freeing its code (D-417); a plan with a published revision stays as it is.
/// # Errors
/// 404; 409 `REVISION_NOT_DRAFT`; 403 `NOT_DRAFT_AUTHOR`; 409 `ITEM_CONFIRMATION_PENDING` while
/// an item's confirm is outstanding; 409 `STALE_REVISION` for a lost race.
pub(super) async fn delete_revision(
    state: Arc<AuthoringState>,
    scope: AccessScope,
    ctx: SecurityContext,
    correlation: Uuid,
    id: Uuid,
) -> Result<Response, CanonicalError> {
    let original_ctx = ctx.clone();
    let ops = support::transaction(&state.db.db(), move |tx| {
        let (scope, ctx) = (scope.clone(), ctx.clone());
        Box::pin(async move {
            let tenant = ctx.subject_tenant_id();
            let children = AccessScope::for_tenant(tenant);
            let m = find_revision(tx, &scope, tenant, id).await?;
            editable(&m, &ctx)?;
            let mut ops = Vec::new();
            for item in plan_item_repo::for_revision(tx, &children, tenant, id).await? {
                ops.push(plan_items::remove(tx, &children, &ctx, correlation, item.id).await?);
            }
            plan_revision_repo::delete_draft(tx, &children, tenant, id, m.version).await?;
            support::audit(tx, &ctx, correlation, "plan_revision.delete", id, m.version).await?;
            // D-417: a never-published plan left without a revision goes with it.
            let p = plan_repo::find(tx, &children, tenant, m.plan_id)
                .await?
                .ok_or_else(|| corrupt(format!("revision {id} has no plan")))?;
            if p.published_rev.is_none()
                && plan_revision_repo::for_plan(tx, &children, tenant, p.id)
                    .await?
                    .is_empty()
            {
                plan_repo::delete_unpublished(tx, &children, tenant, p.id, p.version).await?;
                support::audit(tx, &ctx, correlation, "plan.delete", p.id, p.version).await?;
            }
            Ok(ops)
        })
    })
    .await?;
    plan_items::drive_best_effort(&state, &original_ctx, &ops).await;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `GET /plan-revisions/{id}/checks`: every check on the sale date (D-408, D-482). The stored
/// state is read on a connection, not under a serializable transaction; then every item SKU is
/// read fresh in one `skus_for_write`, never from a cache, so a SKU deprecated since the last
/// read turns its check red at once. A SKU Products no longer knows (404) is unavailable in the
/// checks; any other definite refusal is answered as Products gave it; a registry that cannot
/// answer is 503 `REGISTRY_UNAVAILABLE`.
/// # Errors
/// 404 for a revision the tenant does not hold; the registry's refusal or unavailability.
pub(super) async fn checks(
    state: &AuthoringState,
    scope: AccessScope,
    ctx: SecurityContext,
    id: Uuid,
) -> Result<Response, CanonicalError> {
    let tenant = ctx.subject_tenant_id();
    let today = today();
    let conn = state.db.conn().map_err(DoorError::from)?;
    let mut context = stored_context(&conn, &scope, tenant, id, today).await?;
    // @cpt-begin:cpt-cf-bss-pricing-flow-plans:p1:inst-plans-flow-3
    context.skus = fresh_skus(&state.hub, &ctx, context.items.iter().map(|i| i.sku_id)).await?;
    let body = checks_dto(&context, today);
    // @cpt-end:cpt-cf-bss-pricing-flow-plans:p1:inst-plans-flow-3
    support::response(StatusCode::OK, &body, None)
}
/// `GET /plan-revisions/checks`: the checks of 1 to 50 revisions, each byte-identical to the
/// single read (D-482). One `stored_contexts` and, when any revision is held, one
/// `skus_for_write` over the union of their SKUs. A revision the tenant does not hold, or one
/// outside the caller's plan-read scope, is `missing`. An answer that is all `missing` makes no
/// Products call.
/// # Errors
/// Products' definite refusal as it gave it; 503 `REGISTRY_UNAVAILABLE`.
pub(super) async fn checks_batch(
    state: &AuthoringState,
    scope: AccessScope,
    ctx: SecurityContext,
    ids: Vec<Uuid>,
) -> Result<Response, CanonicalError> {
    let tenant = ctx.subject_tenant_id();
    let today = today();
    let conn = state.db.conn().map_err(DoorError::from)?;
    let mut contexts = stored_contexts(&conn, &scope, tenant, &ids, today).await?;
    if !contexts.is_empty() {
        let named: Vec<Uuid> = contexts
            .values()
            .flat_map(|context| context.items.iter().map(|item| item.sku_id))
            .collect();
        let found = fresh_skus(&state.hub, &ctx, named).await?;
        for context in contexts.values_mut() {
            let wanted: BTreeSet<Uuid> = context.items.iter().map(|item| item.sku_id).collect();
            context.skus = found
                .iter()
                .filter(|sku| wanted.contains(&sku.id))
                .cloned()
                .collect();
        }
    }
    let mut items = Vec::new();
    let mut missing = Vec::new();
    for id in ids {
        match contexts.remove(&id) {
            Some(context) => items.push(dto::PricingRevisionChecksDto {
                revision_id: id,
                checks: checks_dto(&context, today),
            }),
            None => missing.push(id),
        }
    }
    support::response(
        StatusCode::OK,
        &dto::PricingRevisionChecksBatchDto { items, missing },
        None,
    )
}
fn checks_dto(context: &PlanContext, today: time::Date) -> PricingPlanChecksDto {
    let rows = plan::checks(context, today);
    let ready = plan::ready(&rows);
    PricingPlanChecksDto {
        checks: rows.into_iter().map(Into::into).collect(),
        ready,
        sale_date: plan::sale_date(&context.revision, today).to_string(),
        quorum_required: context.quorum,
    }
}
/// Every SKU named, read fresh through one `skus_for_write` and never from a cache (D-408): the
/// checks door, submit and apply all read the item SKUs this way. A SKU Products no longer knows
/// (404) is left out, so the checks show it unavailable.
/// # Errors
/// Any other definite refusal as Products gave it; a registry that cannot answer is 503
/// `REGISTRY_UNAVAILABLE`.
pub async fn fresh_skus(
    hub: &toolkit::ClientHub,
    ctx: &SecurityContext,
    skus: impl IntoIterator<Item = Uuid>,
) -> Result<Vec<Sku>, CanonicalError> {
    let registry =
        reference_registry::resolve(hub).map_err(|e| support::registry_unavailable(&e))?;
    let wanted: Vec<Uuid> = skus
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    // @cpt-begin:cpt-cf-bss-pricing-algo-plans-revision-checks:p1:inst-plans-revision-checks-1
    let found = registry
        .skus_for_write(ctx, ctx.subject_tenant_id(), &wanted)
        .await
        .map_err(|error| {
            if reference_work::definite_refusal(&error) {
                error
            } else {
                support::registry_unavailable(&error)
            }
        })?;
    // @cpt-end:cpt-cf-bss-pricing-algo-plans-revision-checks:p1:inst-plans-revision-checks-1
    Ok(found)
}
fn corrupt(what: String) -> DoorError {
    RepoError::CorruptRow(what).into()
}
/// Everything the checks read from storage, with no SKU yet: the checks door and the
/// `plan_revision` subject build the checks' context through this one function. The plan's
/// revisions are read as they read on `today` (D-447), so the published revision whose SKUs a
/// deprecated SKU may be carried from is the one in effect, before the job persists a due switch
/// as after it.
/// # Errors
/// 404 for a revision the tenant does not hold; storage failures.
pub async fn stored_context(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    today: time::Date,
) -> Result<PlanContext, DoorError> {
    stored_contexts(tx, scope, tenant, &[id], today)
        .await?
        .remove(&id)
        .ok_or_else(|| support::missing_what("plan_revision").into())
}
/// The checks' stored context for every revision among `ids` the caller's scope admits, each
/// built from the same reads as [`stored_context`] (D-482). Eleven statements whatever the number
/// of revisions and entries, once any revision is held: the revisions, their plans, their items,
/// the entries, the prices, the books, the dimensions, the plans' revisions, the in-effect items,
/// the policy and the settings. An id the tenant does not hold, or the scope does not admit, is
/// absent. The caller's scope is used only to admit the revisions; the rest is the tenant's, as
/// the single read's is.
///
/// # Errors
/// Storage failures, including a corrupt row.
pub async fn stored_contexts(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    ids: &[Uuid],
    today: time::Date,
) -> Result<BTreeMap<Uuid, PlanContext>, DoorError> {
    let revisions = plan_revision_repo::find_many(tx, scope, tenant, ids).await?;
    if revisions.is_empty() {
        return Ok(BTreeMap::new());
    }
    let children = AccessScope::for_tenant(tenant);
    let plan_ids: Vec<Uuid> = revisions.iter().map(|r| r.plan_id).collect();
    let plans: BTreeMap<Uuid, plan_entity::Model> =
        plan_repo::find_many(tx, &children, tenant, &plan_ids)
            .await?
            .into_iter()
            .map(|p| (p.id, p))
            .collect();
    let revision_ids: Vec<Uuid> = revisions.iter().map(|r| r.id).collect();
    let mut items_of: BTreeMap<Uuid, Vec<plan_item::Model>> = BTreeMap::new();
    for row in plan_item_repo::for_revisions(tx, &children, tenant, &revision_ids).await? {
        items_of.entry(row.revision_id).or_default().push(row);
    }
    let entry_ids = named_entries(&items_of);
    let entries_by_id: BTreeMap<Uuid, price_book_entry::Model> =
        price_book_entry_repo::find_many(tx, &children, tenant, &entry_ids)
            .await?
            .into_iter()
            .map(|e| (e.id, e))
            .collect();
    let prices_by_entry =
        price_repo::by_entry(price_repo::for_entries(tx, &children, tenant, &entry_ids).await?);
    let book_ids = named_books(&revisions, &entries_by_id);
    let books_by_id: BTreeMap<Uuid, plan::PlanBook> =
        book_repo::find_many(tx, &children, tenant, &book_ids)
            .await?
            .into_iter()
            .map(|b| {
                (
                    b.id,
                    plan::PlanBook {
                        id: b.id,
                        book: Book {
                            name: b.name,
                            currency: b.currency,
                            valid_from: b.valid_from,
                            valid_until: b.valid_until,
                        },
                    },
                )
            })
            .collect();
    let mut dimension_values = Vec::new();
    for d in dimension_repo::list(tx, &children, tenant).await? {
        let values: Vec<String> = serde_json::from_value(d.values)
            .map_err(|_| corrupt(format!("dimension {} values", d.key)))?;
        dimension_values.push((d.key, values));
    }
    let mut siblings_of: BTreeMap<Uuid, Vec<plan_revision::Model>> = BTreeMap::new();
    for row in plan_revision_repo::for_plans(tx, &children, tenant, &plan_ids).await? {
        siblings_of.entry(row.plan_id).or_default().push(row);
    }
    let effective = effective_plans(&siblings_of, today)?;
    let effective_of = effective.by_plan;
    let in_effect_ids = effective.in_effect_ids;
    let published_skus =
        plan_item_repo::skus_of_revisions(tx, &children, tenant, &in_effect_ids).await?;
    let quorum = approval_repo::read_policy(tx, &children, tenant)
        .await?
        .quorum_for(plan::KIND_PLAN_REVISION);
    let settings = configuration::settings(tx, &children, tenant).await?;
    let defaults = plan::Defaults {
        gl: settings.default_gl,
        rounding: settings.default_rounding,
        tax_category: settings.default_tax_category,
    };
    let loaded = Loaded {
        plans: &plans,
        items_of: &items_of,
        entries_by_id: &entries_by_id,
        prices_by_entry: &prices_by_entry,
        books_by_id: &books_by_id,
        effective_of: &effective_of,
        published_skus: &published_skus,
        dimension_values: &dimension_values,
        quorum,
        defaults: &defaults,
    };
    let mut out = BTreeMap::new();
    for revision in revisions {
        let id = revision.id;
        out.insert(id, assemble(&revision, &loaded)?);
    }
    Ok(out)
}
struct Loaded<'a> {
    plans: &'a BTreeMap<Uuid, plan_entity::Model>,
    items_of: &'a BTreeMap<Uuid, Vec<plan_item::Model>>,
    entries_by_id: &'a BTreeMap<Uuid, price_book_entry::Model>,
    prices_by_entry: &'a BTreeMap<Uuid, Vec<price::Model>>,
    books_by_id: &'a BTreeMap<Uuid, plan::PlanBook>,
    effective_of: &'a BTreeMap<Uuid, Vec<plan::EffectiveRevision>>,
    published_skus: &'a BTreeMap<Uuid, Vec<Uuid>>,
    dimension_values: &'a [(String, Vec<String>)],
    quorum: u32,
    defaults: &'a plan::Defaults,
}
fn assemble(
    revision: &plan_revision::Model,
    loaded: &Loaded<'_>,
) -> Result<PlanContext, DoorError> {
    let plan_row = loaded
        .plans
        .get(&revision.plan_id)
        .ok_or_else(|| corrupt(format!("revision {} has no plan", revision.id)))?;
    let effective = loaded
        .effective_of
        .get(&revision.plan_id)
        .ok_or_else(|| corrupt(format!("revision {} is not among its plan's", revision.id)))?;
    let state = effective
        .iter()
        .find(|x| x.id == revision.id)
        .map(|x| x.state)
        .ok_or_else(|| corrupt(format!("revision {} is not among its plan's", revision.id)))?;
    let published_sku_ids = plan::in_effect(effective)
        .and_then(|published| loaded.published_skus.get(&published.id).cloned())
        .unwrap_or_default();
    let rows = rows_of(revision, loaded)?;
    Ok(PlanContext {
        plan: plan::Plan {
            id: plan_row.id,
            code: plan_row.code.clone(),
            name: plan_row.name.clone(),
        },
        revision: plan::Revision {
            id: revision.id,
            rev_no: revision.rev_no,
            book_id: revision.book_id,
            state,
            available_from: revision.available_from,
        },
        items: rows.items,
        skus: Vec::new(),
        entries: rows.entries,
        books: rows.books,
        dimension_values: loaded.dimension_values.to_vec(),
        published_sku_ids,
        quorum: loaded.quorum,
        defaults: loaded.defaults.clone(),
    })
}
fn named_entries(items_of: &BTreeMap<Uuid, Vec<plan_item::Model>>) -> Vec<Uuid> {
    let mut entry_ids = BTreeSet::new();
    for rows in items_of.values() {
        for row in rows {
            if let Some(id) = row.price_book_entry_id {
                entry_ids.insert(id);
            }
        }
    }
    entry_ids.into_iter().collect()
}
fn named_books(
    revisions: &[plan_revision::Model],
    entries: &BTreeMap<Uuid, price_book_entry::Model>,
) -> Vec<Uuid> {
    let mut book_ids = BTreeSet::new();
    for revision in revisions {
        book_ids.insert(revision.book_id);
    }
    for entry in entries.values() {
        book_ids.insert(entry.book_id);
    }
    book_ids.into_iter().collect()
}
struct EffectivePlans {
    by_plan: BTreeMap<Uuid, Vec<plan::EffectiveRevision>>,
    in_effect_ids: Vec<Uuid>,
}
fn effective_plans(
    siblings: &BTreeMap<Uuid, Vec<plan_revision::Model>>,
    today: time::Date,
) -> Result<EffectivePlans, DoorError> {
    let mut by_plan = BTreeMap::new();
    let mut in_effect_ids = Vec::new();
    for (plan_id, rows) in siblings {
        let effective = plan_revisions::effective_revisions(rows, today)?;
        if let Some(published) = plan::in_effect(&effective) {
            in_effect_ids.push(published.id);
        }
        by_plan.insert(*plan_id, effective);
    }
    Ok(EffectivePlans {
        by_plan,
        in_effect_ids,
    })
}
struct RevisionRows {
    items: Vec<plan::Item>,
    entries: Vec<plan::Entry>,
    books: Vec<plan::PlanBook>,
}
fn rows_of(
    revision: &plan_revision::Model,
    loaded: &Loaded<'_>,
) -> Result<RevisionRows, DoorError> {
    let rows = match loaded.items_of.get(&revision.id) {
        Some(rows) => rows.as_slice(),
        None => &[],
    };
    let mut items = Vec::with_capacity(rows.len());
    let mut entries = Vec::new();
    let mut seen_entries = BTreeSet::new();
    let mut revision_books = BTreeSet::from([revision.book_id]);
    for row in rows {
        items.push(item_of(row)?);
        let Some(entry_id) = row.price_book_entry_id else {
            continue;
        };
        if !seen_entries.insert(entry_id) {
            continue;
        }
        if let Some(entry) = loaded.entries_by_id.get(&entry_id) {
            revision_books.insert(entry.book_id);
            let prices = match loaded.prices_by_entry.get(&entry_id) {
                Some(prices) => prices.as_slice(),
                None => &[],
            };
            entries.push(entry_of(entry, prices)?);
        }
    }
    let books = revision_books
        .into_iter()
        .filter_map(|id| loaded.books_by_id.get(&id).cloned())
        .collect();
    Ok(RevisionRows {
        items,
        entries,
        books,
    })
}
fn item_of(m: &plan_item::Model) -> Result<plan::Item, DoorError> {
    let bad = |what: &str| corrupt(format!("plan item {} {what}", m.id));
    Ok(plan::Item {
        id: m.id,
        sku_id: m.sku_id,
        price_book_entry_id: m.price_book_entry_id,
        reference: plan::Reference {
            state: m
                .reference_state
                .parse()
                .map_err(|_| bad("reference_state"))?,
            reservation_id: m.reservation_id,
        },
    })
}
fn entry_of(
    e: &price_book_entry::Model,
    stored: &[price::Model],
) -> Result<plan::Entry, DoorError> {
    let bad = |what: &str| corrupt(format!("entry {} {what}", e.id));
    // A `cancel` or `end` is not a price: it never covers an item, pending or applied
    // (D-520, D-521).
    let stored: Vec<&price::Model> = stored.iter().filter(|m| price_repo::is_price(m)).collect();
    let pending = stored
        .iter()
        .filter(|p| p.state == PriceState::Pending.as_str())
        .filter_map(|p| {
            p.pending_unit_id.map(|unit_id| plan::PendingPrice {
                price_id: p.id,
                unit_id,
            })
        })
        .collect();
    let model = price_book_entry_repo::model_of(e)?;
    let prices = stored
        .iter()
        .map(|m| price_repo::to_domain(m, model))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(plan::Entry {
        id: e.id,
        book_id: e.book_id,
        sku_id: e.sku_id,
        charge_kind: e.charge_kind.parse().map_err(|_| bad("charge_kind"))?,
        period: e.period.clone(),
        dimension_key: e.dimension_key.clone(),
        reference_state: e
            .reference_state
            .parse()
            .map_err(|_| bad("reference_state"))?,
        prices,
        pending,
    })
}
