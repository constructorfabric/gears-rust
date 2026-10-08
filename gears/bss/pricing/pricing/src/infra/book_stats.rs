//! A book's stats (D-441): what every book read carries for the Price Books screen.
//!
//! A fixed number of grouped statements whatever the number of books on the page, one per source
//! and none multiplying another: the entries (count, distinct SKUs, latest change), the prices of
//! those entries (by state, the approved ones by where their window stands today, latest change),
//! the plans that name the book (those with a live revision on it, and those only superseded
//! revisions keep there), and the book's `prices` units (pending, latest submission or decision). Every source is read tenant-scoped: the counts are facts of a book
//! the caller may read (`price_book` read), as D-428 reads an entry's usage.
use crate::domain::price::PriceState;
use crate::infra::storage::{
    RepoError,
    entity::price_book,
    repo::{self, approval_repo, plan_revision_repo, price_book_entry_repo, price_repo},
};
use crate::infra::usage::count;
use std::collections::BTreeMap;
use toolkit_db::secure::DBRunner;
use uuid::Uuid;

/// A book's prices by state — a rejected price included, a cancelled price not (D-520: it is
/// not a price in force) — and its approved prices by where their window stands today:
/// `approved` = `scheduled + active + superseded`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BookPriceCounts {
    pub draft: u64,
    pub pending: u64,
    pub approved: u64,
    pub scheduled: u64,
    pub active: u64,
    pub superseded: u64,
    pub rejected: u64,
}
/// One book's stats (D-441).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BookStats {
    /// The book's entries, in every reference state.
    pub entries: u64,
    /// The distinct SKUs of those entries.
    pub skus: u64,
    /// The distinct plans with a draft, pending, scheduled or published revision on the book
    /// ([`plan_revision_repo::plans_on_books`]).
    pub plans: u64,
    /// The distinct plans that name the book only through superseded revisions (the same read):
    /// the book delete's `BOOK_IN_PLAN_HISTORY` (D-444). The delete succeeds exactly when this,
    /// `plans` and `entries` are 0.
    pub plans_superseded_only: u64,
    pub prices: BookPriceCounts,
    /// The book's `prices` units in review.
    pub pending_units: u64,
    /// The latest of the book's `updated_at`, its entries' and their prices' `updated_at`, and its
    /// `prices` units' submissions and decisions. A deleted draft or entry leaves nothing to read,
    /// so its deletion does not move it.
    pub last_change_at: time::OffsetDateTime,
}

/// The stats of each of `books` (the tenant's), keyed by book id, dated on `today`: four grouped
/// statements whatever the number of books; none for an empty page.
/// # Errors
/// Storage failures; a stored price state pricing does not know, a negative count or a latest
/// instant that does not parse is a corrupt row.
pub async fn book_stats(
    runner: &impl DBRunner,
    tenant: Uuid,
    backend: sea_orm::DbBackend,
    books: &[&price_book::Model],
    today: time::Date,
) -> Result<BTreeMap<Uuid, BookStats>, RepoError> {
    let ids: Vec<Uuid> = books.iter().map(|b| b.id).collect();
    let mut out: BTreeMap<Uuid, BookStats> = books
        .iter()
        .map(|b| {
            (
                b.id,
                BookStats {
                    entries: 0,
                    skus: 0,
                    plans: 0,
                    plans_superseded_only: 0,
                    prices: BookPriceCounts::default(),
                    pending_units: 0,
                    last_change_at: b.updated_at,
                },
            )
        })
        .collect();
    let later = |stats: &mut BookStats, text: Option<&str>| -> Result<(), RepoError> {
        if let Some(at) = repo::latest_instant(text)? {
            stats.last_change_at = stats.last_change_at.max(at);
        }
        Ok(())
    };
    for row in price_book_entry_repo::count_by_book(runner, tenant, backend, &ids).await? {
        if let Some(stats) = out.get_mut(&row.book_id) {
            stats.entries = count(row.entries)?;
            stats.skus = count(row.skus)?;
            later(stats, row.latest.as_deref())?;
        }
    }
    for row in price_repo::count_by_book_and_state(runner, tenant, backend, &ids, today).await? {
        let Some(book) = out.get_mut(&row.book_id) else {
            continue;
        };
        let n = count(row.count)?;
        let state: PriceState = row
            .state
            .parse()
            .map_err(|_| RepoError::CorruptRow(format!("unknown price state {}", row.state)))?;
        let p = &mut book.prices;
        match state {
            PriceState::Draft => p.draft += n,
            PriceState::Pending => p.pending += n,
            PriceState::Rejected => p.rejected += n,
            PriceState::Approved => {
                let (ended, future) = (count(row.ended)?, count(row.future)?);
                let active = n.checked_sub(ended + future).ok_or_else(|| {
                    RepoError::CorruptRow(format!(
                        "{ended} ended and {future} future of {n} prices"
                    ))
                })?;
                p.approved += n;
                p.superseded += ended;
                p.scheduled += future;
                p.active += active;
            }
            PriceState::Cancelled => {}
        }
        later(book, row.latest.as_deref())?;
    }
    for row in plan_revision_repo::plans_on_books(runner, tenant, &ids).await? {
        if let Some(stats) = out.get_mut(&row.book_id) {
            let (plans, named) = (count(row.plans)?, count(row.named)?);
            stats.plans = plans;
            stats.plans_superseded_only = named.checked_sub(plans).ok_or_else(|| {
                RepoError::CorruptRow(format!("{plans} live of {named} plans on a book"))
            })?;
        }
    }
    for row in approval_repo::prices_units_by_book(runner, tenant, backend, &ids).await? {
        if let Some(stats) = out.get_mut(&row.ref_id) {
            stats.pending_units = count(row.pending)?;
            later(stats, row.latest.as_deref())?;
        }
    }
    Ok(out)
}
