//! Book writes share one transaction with their audit and POST receipt; a book carries an
//! optional description and an unused one is deleted (D-444); the book reads carry their stats
//! (D-441) and the list pages on the toolkit's `OData` pager (D-442).
//!
//! @cpt-dod:cpt-cf-bss-pricing-dod-book-currency-validity:p1
//! @cpt-dod:cpt-cf-bss-pricing-dod-book-export:p1
use super::{
    dto::{
        PriceBookCreate, PriceBookDto, PriceBookExport, PriceBookPatch, PricingExportEntry,
        PricingPriceBookReadDto, PricingPriceDto,
    },
    support::{DoorError, audit, check_version, conflict, date, invalid, missing, response, value},
};
use crate::{
    domain::book,
    infra::storage::{
        entity::price_book,
        repo::{
            book_repo, idempotency_repo as idem, plan_revision_repo, price_book_entry_repo,
            price_repo, reference_op_repo,
        },
    },
};
use axum::{http::StatusCode, response::Response};
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::secure::{AccessScope, DBRunner};
use toolkit_security::SecurityContext;
use uuid::Uuid;
pub async fn find(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
) -> Result<price_book::Model, DoorError> {
    book_repo::find(tx, scope, tenant, id)
        .await?
        .ok_or_else(|| missing().into())
}
fn validate(m: &price_book::Model) -> Result<(), CanonicalError> {
    if m.code.trim().is_empty() {
        return Err(invalid("code", "BOOK_CODE_REQUIRED"));
    }
    let errors = book::validate(&book::Book {
        name: m.name.clone(),
        currency: m.currency.clone(),
        valid_from: m.valid_from,
        valid_until: m.valid_until,
    });
    if let Some(e) = errors.first() {
        return Err(invalid("book", e.code));
    }
    book::validate_description(m.description.as_deref()).map_err(|e| invalid("description", e.code))
}
pub async fn create(
    tx: &impl DBRunner,
    scope: &AccessScope,
    ctx: &SecurityContext,
    correlation: Uuid,
    key: &str,
    digest: &[u8],
    body: PriceBookCreate,
) -> Result<Response, DoorError> {
    let tenant = ctx.subject_tenant_id();
    let now = crate::infra::storage::stored_now();
    let receipt_scope = AccessScope::for_tenant(tenant);
    let endpoint = "/bss-pricing/v1/price-books";
    let claim = idem::claim_idempotency_key(
        tx,
        &receipt_scope,
        tenant,
        endpoint,
        key,
        digest,
        now,
        now + time::Duration::hours(24),
    )
    .await?;
    // The book's answer is stored as its body, with its version inside it.
    if let Some((status, body)) = super::support::held(claim, digest)? {
        return Ok(response(
            super::support::stored_status(status)?,
            &body,
            body["version"].as_u64(),
        )?);
    }
    let model = price_book::Model {
        id: Uuid::now_v7(),
        tenant_id: tenant,
        code: body.code,
        name: body.name,
        currency: body.currency,
        valid_from: date(body.valid_from, "valid_from")?,
        valid_until: date(body.valid_until, "valid_until")?,
        description: body.description,
        version: 1,
        created_at: now,
        updated_at: now,
        archived_at: None,
        archived_by: None,
    };
    validate(&model)?;
    // D-438: a new book takes a currency the tenant offers (any, while it offers none).
    super::configuration::offer_currency(tx, tenant, &model.currency).await?;
    let model = book_repo::insert(tx, scope, model).await?;
    audit(tx, ctx, correlation, "price_book.create", model.id, 1).await?;
    let body = value(&PriceBookDto::from(model))?;
    if idem::answer_idempotency_key(
        tx,
        &receipt_scope,
        tenant,
        endpoint,
        key,
        201,
        body.clone(),
        None,
    )
    .await?
        != idem::IdempotencyAnswer::Recorded
    {
        return Err(CanonicalError::internal("idempotency claim lost")
            .create()
            .into());
    }
    Ok(response(StatusCode::CREATED, &body, Some(1))?)
}
pub async fn patch(
    tx: &impl DBRunner,
    scope: &AccessScope,
    ctx: &SecurityContext,
    correlation: Uuid,
    id: Uuid,
    version: u64,
    body: PriceBookPatch,
) -> Result<Response, DoorError> {
    let mut m = find(tx, scope, ctx.subject_tenant_id(), id).await?;
    check_version(version, m.version)?;
    if let Some(name) = body.name {
        m.name = name;
    }
    if let Some(from) = body.valid_from {
        m.valid_from = date(from, "valid_from")?;
    }
    if let Some(until) = body.valid_until {
        m.valid_until = date(until, "valid_until")?;
    }
    if let Some(description) = body.description {
        m.description = description;
    }
    validate(&m)?;
    m.updated_at = crate::infra::storage::stored_now();
    book_repo::update(tx, scope, m.clone()).await?;
    m.version += 1;
    audit(tx, ctx, correlation, "price_book.update", id, m.version).await?;
    Ok(response(
        StatusCode::OK,
        &PriceBookDto::from(m),
        Some(version + 1),
    )?)
}
/// `DELETE /price-books/{id}` (D-444): an unused book at the version the caller read, with an
/// audit row. Refused in this order, after the door's authorization and If-Match: 404 for a book
/// the tenant does not hold; 409 `STALE_REVISION`; 409 `BOOK_HAS_ENTRIES` for an entry of any
/// reference state; 409 `BOOK_IN_PLAN` for a plan with a draft, pending, scheduled or published
/// revision on it; 409 `BOOK_IN_PLAN_HISTORY` when only superseded revisions name it (their
/// history keeps the book). Both are judged from `plan_revision_repo::plans_on_books`, the read
/// `stats.plans` and `stats.plans_superseded_only` count (D-441), and the entries by the read
/// `stats.entries` counts, so the delete succeeds exactly when the three are 0. No unit can be
/// pending on a book without entries (a pending price keeps its entry), so there is no refusal of
/// its own for one. A row a concurrent writer adds after these reads is the same 409, from the
/// book's foreign key (`book_repo::delete`). Units that named the book stay, and their cards answer
/// without it.
/// # Errors
/// The refusals above; storage failures.
pub async fn delete(
    tx: &impl DBRunner,
    scope: &AccessScope,
    ctx: &SecurityContext,
    correlation: Uuid,
    backend: sea_orm::DbBackend,
    id: Uuid,
    version: u64,
) -> Result<Response, DoorError> {
    let tenant = ctx.subject_tenant_id();
    let m = find(tx, scope, tenant, id).await?;
    check_version(version, m.version)?;
    let entries = price_book_entry_repo::count_by_book(tx, tenant, backend, &[id]).await?;
    if !entries.is_empty() {
        return Err(conflict("BOOK_HAS_ENTRIES").into());
    }
    let plans = plan_revision_repo::plans_on_books(tx, tenant, &[id]).await?;
    if plans.iter().any(|row| row.plans > 0) {
        return Err(conflict("BOOK_IN_PLAN").into());
    }
    if plans.iter().any(|row| row.named > 0) {
        return Err(conflict("BOOK_IN_PLAN_HISTORY").into());
    }
    book_repo::delete(tx, scope, tenant, id, m.version).await?;
    audit(tx, ctx, correlation, "price_book.delete", id, m.version).await?;
    Ok(axum::response::IntoResponse::into_response(
        StatusCode::NO_CONTENT,
    ))
}
/// What an archive or an unarchive wrote (D-522): the book as it now stands, and the reference
/// ops the door drives after the commit (the ticker finishes what it does not).
pub struct Marked {
    pub book: price_book::Model,
    pub ops: Vec<Uuid>,
}
/// `POST /price-books/{id}/archive` (D-522): a finished book, at the version the caller read
/// (If-Match), with an audit row. Refused in this order, after the door's authorization and
/// If-Match: 404 for a book the tenant does not hold; 409 `STALE_REVISION`; 409 `BOOK_IN_PLAN`
/// for a plan with a draft, pending, scheduled or published revision on it (superseded ones do not
/// refuse it: they are history); 409 `BOOK_HAS_PENDING` while a `prices` unit of the book is in
/// review (a pending price, cancel or end, as the stats' `pending_units` count it); 409
/// `ENTRY_CONFIRMATION_PENDING` while an entry's reference is being confirmed. An archived book is
/// answered as it is, and nothing is written.
///
/// In the same transaction every `confirmed` or `lost` entry becomes `released`, with a `release`
/// op for its reservation ([`crate::infra::reference_work::release_op`], reason `book_archived`); the
/// door drives
/// them after the commit. The entries and the prices stay; from now on they are read-only.
/// # Errors
/// The refusals above; storage failures.
pub async fn archive(
    tx: &impl DBRunner,
    scope: &AccessScope,
    ctx: &SecurityContext,
    correlation: Uuid,
    backend: sea_orm::DbBackend,
    id: Uuid,
    version: u64,
) -> Result<Marked, DoorError> {
    use crate::domain::price_book_entry::ReferenceState;
    let tenant = ctx.subject_tenant_id();
    let mut m = find(tx, scope, tenant, id).await?;
    check_version(version, m.version)?;
    if m.archived_at.is_some() {
        return Ok(Marked {
            book: m,
            ops: Vec::new(),
        });
    }
    let plans = plan_revision_repo::plans_on_books(tx, tenant, &[id]).await?;
    if plans.iter().any(|row| row.plans > 0) {
        return Err(conflict("BOOK_IN_PLAN").into());
    }
    // A pending price, cancel or end is in a `prices` unit under review: the one read the stats'
    // `pending_units` counts (D-441).
    if crate::infra::storage::repo::approval_repo::prices_units_by_book(tx, tenant, backend, &[id])
        .await?
        .iter()
        .any(|row| row.pending > 0)
    {
        return Err(conflict("BOOK_HAS_PENDING").into());
    }
    // The entries are the book's aggregate; read tenant-scoped, as the export reads them.
    let children = AccessScope::for_tenant(tenant);
    let entries = price_book_entry_repo::for_book(tx, &children, tenant, id).await?;
    if entries
        .iter()
        .any(|e| e.reference_state == ReferenceState::ConfirmationPending.as_str())
    {
        return Err(conflict("ENTRY_CONFIRMATION_PENDING").into());
    }
    let now = crate::infra::storage::stored_now();
    book_repo::set_archived(
        tx,
        scope,
        tenant,
        id,
        m.version,
        Some(ctx.subject_id()),
        now,
    )
    .await?;
    m.version += 1;
    m.archived_at = Some(now);
    m.archived_by = Some(ctx.subject_id());
    m.updated_at = now;
    audit(tx, ctx, correlation, "price_book.archive", id, m.version).await?;
    let held = |e: &&crate::infra::storage::entity::price_book_entry::Model| {
        e.reference_state == ReferenceState::Confirmed.as_str()
            || e.reference_state == ReferenceState::Lost.as_str()
    };
    let ops = entries
        .iter()
        .filter(held)
        .map(|e| crate::infra::reference_work::release_op(ctx, e, correlation, now))
        .collect::<Result<Vec<_>, _>>()?;
    let ids = ops.iter().map(|op| op.op_id).collect();
    price_book_entry_repo::release_book(tx, &children, tenant, id, now).await?;
    reference_op_repo::insert_all(tx, &children, ops).await?;
    Ok(Marked { book: m, ops: ids })
}
/// `POST /price-books/{id}/unarchive` (D-522): the archive mark cleared at the version the caller
/// read (If-Match), with an audit row; 404 and 409 `STALE_REVISION` as the archive refuses them. A
/// book that is not archived is answered as it is, and nothing is written. Then 409
/// `ENTRY_RELEASE_PENDING` while an entry of the book has an open `release` or `rereserve` op (the
/// mirror of the archive's `ENTRY_CONFIRMATION_PENDING`), and nothing is written: an entry skipped
/// here would stay `released` and read-only once its release finished.
///
/// In the same transaction each `released` entry gets a `rereserve` op; the door drives them after
/// the commit. An entry whose SKU refuses the new reservation (retired, say) stays `released` and
/// read-only, and the book is unarchived anyway.
/// # Errors
/// The refusals above; storage failures.
pub async fn unarchive(
    tx: &impl DBRunner,
    scope: &AccessScope,
    ctx: &SecurityContext,
    correlation: Uuid,
    id: Uuid,
    version: u64,
) -> Result<Marked, DoorError> {
    use crate::domain::price_book_entry::ReferenceState;
    use crate::domain::reference_op::{OpKind, RefKind};
    let tenant = ctx.subject_tenant_id();
    let mut m = find(tx, scope, tenant, id).await?;
    check_version(version, m.version)?;
    if m.archived_at.is_none() {
        return Ok(Marked {
            book: m,
            ops: Vec::new(),
        });
    }
    // The entries are the book's aggregate; read tenant-scoped, as the archive reads them.
    let children = AccessScope::for_tenant(tenant);
    let entries = price_book_entry_repo::for_book(tx, &children, tenant, id).await?;
    let open = reference_op_repo::open_refs(
        tx,
        &children,
        tenant,
        RefKind::Entry,
        &entries.iter().map(|e| e.id).collect::<Vec<_>>(),
        &[OpKind::Release, OpKind::Rereserve],
    )
    .await?;
    if !open.is_empty() {
        return Err(conflict("ENTRY_RELEASE_PENDING").into());
    }
    let now = crate::infra::storage::stored_now();
    book_repo::set_archived(tx, scope, tenant, id, m.version, None, now).await?;
    m.version += 1;
    m.archived_at = None;
    m.archived_by = None;
    m.updated_at = now;
    audit(tx, ctx, correlation, "price_book.unarchive", id, m.version).await?;
    let due = now + crate::infra::reference_work::IN_FLIGHT_GRACE;
    let ops = entries
        .iter()
        .filter(|e| e.reference_state == ReferenceState::Released.as_str())
        .map(|e| crate::infra::reference_work::rereserve_op(ctx, e, now, due))
        .collect::<Result<Vec<_>, _>>()?;
    let ids = ops.iter().map(|op| op.op_id).collect();
    reference_op_repo::insert_all(tx, &children, ops).await?;
    Ok(Marked { book: m, ops: ids })
}
/// The entries of book `id` that read `released` now (D-522): an unarchive's answer.
/// # Errors
/// Storage failures.
pub async fn released_entries(
    tx: &impl DBRunner,
    tenant: Uuid,
    id: Uuid,
) -> Result<Vec<Uuid>, DoorError> {
    use crate::domain::price_book_entry::ReferenceState;
    let mut ids: Vec<Uuid> =
        price_book_entry_repo::for_book(tx, &AccessScope::for_tenant(tenant), tenant, id)
            .await?
            .into_iter()
            .filter(|e| e.reference_state == ReferenceState::Released.as_str())
            .map(|e| e.id)
            .collect();
    ids.sort_unstable();
    Ok(ids)
}
/// Each of the tenant's `books` with its stats dated on `today` (D-441), in their order: four
/// grouped statements whatever their number.
/// # Errors
/// Storage failures and corrupt rows.
pub async fn with_stats(
    tx: &impl DBRunner,
    tenant: Uuid,
    backend: sea_orm::DbBackend,
    books: Vec<price_book::Model>,
    today: time::Date,
) -> Result<Vec<PricingPriceBookReadDto>, DoorError> {
    let mut stats = crate::infra::book_stats::book_stats(
        tx,
        tenant,
        backend,
        &books.iter().collect::<Vec<_>>(),
        today,
    )
    .await?;
    books
        .into_iter()
        .map(|b| {
            let counted = stats.remove(&b.id).ok_or_else(|| {
                CanonicalError::internal(format!("no stats for book {}", b.id)).create()
            })?;
            Ok(PricingPriceBookReadDto {
                book: b.into(),
                stats: counted.into(),
            })
        })
        .collect()
}
/// `GET /price-books` (D-442): one page of the tenant's books under the caller's `scope`,
/// narrowed by `filter` and the query, each with its stats (D-441): the page's statement and the
/// stats' four.
/// # Errors
/// 400 for a query the pager refuses; storage failures and corrupt rows.
pub async fn page(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    backend: sea_orm::DbBackend,
    filter: &book_repo::BookListFilter,
    query: &toolkit_odata::ODataQuery,
    today: time::Date,
) -> Result<toolkit_odata::Page<PricingPriceBookReadDto>, DoorError> {
    let page = book_repo::page(tx, scope, tenant, backend, filter, query)
        .await
        .map_err(|e| match e {
            book_repo::BookListError::Query(e) => DoorError::Api(e.into()),
            book_repo::BookListError::Repo(e) => DoorError::Repo(e),
        })?;
    Ok(toolkit_odata::Page {
        items: with_stats(tx, tenant, backend, page.items, today).await?,
        page_info: page.page_info,
    })
}
/// One page of book `id`'s entries under the caller's entry `scope` (D-483): the query's `$filter`
/// and cursor, ordered `(sku_id, charge_kind, model, id)`, 500 by default and at most 500. ONE
/// statement; the door has found the book.
/// # Errors
/// 400 for a filter value or a cursor the pager refuses; storage failures.
pub async fn entries_page(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
    query: &toolkit_odata::ODataQuery,
) -> Result<toolkit_odata::Page<crate::infra::storage::entity::price_book_entry::Model>, DoorError>
{
    price_book_entry_repo::page_of_book(tx, scope, tenant, id, query)
        .await
        .map_err(|e| match e {
            price_book_entry_repo::EntryListError::Query(e) => DoorError::Api(e.into()),
            price_book_entry_repo::EntryListError::Repo(e) => DoorError::Repo(e),
        })
}
/// Every entry of book `id` (404 for a book the tenant does not hold), in the export's order
/// `(sku_id, charge_kind, period or "", id)`: the export reads the whole book.
pub async fn entries(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
) -> Result<Vec<crate::infra::storage::entity::price_book_entry::Model>, DoorError> {
    find(tx, &AccessScope::for_tenant(tenant), tenant, id).await?;
    let mut entries = price_book_entry_repo::for_book(tx, scope, tenant, id).await?;
    entries.sort_by(|a, b| {
        (
            a.sku_id,
            &a.charge_kind,
            a.period.as_deref().unwrap_or(""),
            a.id,
        )
            .cmp(&(
                b.sku_id,
                &b.charge_kind,
                b.period.as_deref().unwrap_or(""),
                b.id,
            ))
    });
    Ok(entries)
}
pub async fn export(
    tx: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    id: Uuid,
) -> Result<PriceBookExport, DoorError> {
    let book = find(tx, scope, tenant, id).await?;
    let mut result = Vec::new();
    // The authorized book is the export aggregate; subordinate IDs are not book IDs.
    let children = AccessScope::for_tenant(tenant);
    let entries = entries(tx, &children, tenant, id).await?;
    // The book's prices in ONE statement (PS-16).
    let mut grouped = price_repo::by_entry(
        price_repo::for_entries(
            tx,
            &children,
            tenant,
            &entries.iter().map(|p| p.id).collect::<Vec<_>>(),
        )
        .await?,
    );
    let mut policies =
        crate::infra::storage::repo::usage_policy_repo::for_entries(tx, tenant, &entries).await?;
    for p in entries {
        let mut prices = grouped.remove(&p.id).unwrap_or_default();
        prices.sort_by(|a, b| {
            (&a.dim_value, a.effective_from, a.version_no, a.id).cmp(&(
                &b.dim_value,
                b.effective_from,
                b.version_no,
                b.id,
            ))
        });
        // Every price echoes its entry's model (D-427).
        let model = p.model.clone();
        let policy = policies.remove(&p.id);
        let entry = super::dto::PricingPriceBookEntryDto::from_stored(p, policy)?;
        result.push(PricingExportEntry {
            entry,
            prices: prices
                .into_iter()
                .map(|m| PricingPriceDto::of(m, &model))
                .collect::<Result<_, _>>()?,
        });
    }
    Ok(PriceBookExport {
        book: book.into(),
        entries: result,
    })
}
