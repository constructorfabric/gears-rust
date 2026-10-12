//! Usage counts of entries and of SKUs (D-428): what the two entry reads carry, and what the SKU
//! usage port answers Products (P-D-197).
//!
//! Every read here is set-based, so a request makes the same number of statements whatever the
//! number of entries or SKUs: one grouped count of the entries' prices (by state, and the approved
//! ones by where their window stands today, D-440), one read of the plan items naming them, one
//! read of those items' revisions — and, for SKUs, one read of their entries and one of those
//! entries' books before that.
use crate::domain::{plan::RevisionState, price::PriceState};
use crate::infra::storage::{
    RepoError,
    repo::{book_repo, plan_item_repo, plan_revision_repo, price_book_entry_repo, price_repo},
};
use bss_products_sdk::sku_usage::{PriceCounts, SkuUsage, SkuUsageSets};
use std::collections::{BTreeMap, BTreeSet};
use toolkit_db::secure::{AccessScope, DBRunner};
use uuid::Uuid;

/// An entry's prices by state, a rejected or cancelled price not counted (D-428, D-520), and its approved prices by
/// where their window stands on the day asked (D-440): `approved` is always `scheduled + active +
/// superseded`. Pricing's own: the SKU usage port answers products-sdk's `PriceCounts`, unchanged.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EntryPriceCounts {
    pub approved: u64,
    pub pending: u64,
    pub draft: u64,
    /// Approved, starting after the day.
    pub scheduled: u64,
    /// Approved, in force on the day.
    pub active: u64,
    /// Approved, ended on or before the day.
    pub superseded: u64,
}
impl From<EntryPriceCounts> for PriceCounts {
    fn from(c: EntryPriceCounts) -> Self {
        Self {
            approved: c.approved,
            pending: c.pending,
            draft: c.draft,
        }
    }
}
/// An entry's usage (D-428).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EntryUsage {
    /// The entry's prices by state, and its approved ones by date.
    pub prices: EntryPriceCounts,
    /// The distinct plans with a draft, pending, scheduled or published revision whose items name
    /// the entry.
    pub plans: u64,
    /// The distinct plans that name the entry only through superseded revisions.
    pub plans_superseded_only: u64,
}
/// What one entry is counted from.
#[derive(Default)]
struct Tally {
    prices: EntryPriceCounts,
    /// Plans with a draft, pending, scheduled or published revision naming the entry.
    live: BTreeSet<Uuid>,
    /// Plans with a superseded revision naming the entry.
    superseded: BTreeSet<Uuid>,
}
fn size<T>(set: impl Iterator<Item = T>) -> u64 {
    u64::try_from(set.count()).unwrap_or(u64::MAX)
}
/// A grouped count as a `u64`.
/// # Errors
/// `CorruptRow` for a negative count.
pub fn count(n: i64) -> Result<u64, RepoError> {
    u64::try_from(n).map_err(|_| RepoError::CorruptRow(format!("a negative count {n}")))
}
/// The prices and the plans of each entry, read tenant-scoped: the counts are facts of an entry
/// the caller may read, and need no price or plan read of their own.
async fn tallies(
    runner: &impl DBRunner,
    tenant: Uuid,
    entries: &[Uuid],
    today: time::Date,
) -> Result<BTreeMap<Uuid, Tally>, RepoError> {
    let scope = AccessScope::for_tenant(tenant);
    let mut out: BTreeMap<Uuid, Tally> = entries.iter().map(|id| (*id, Tally::default())).collect();
    for row in price_repo::count_by_entry_and_state(runner, &scope, tenant, entries, today).await? {
        let n = count(row.count)?;
        let state: PriceState = row
            .state
            .parse()
            .map_err(|_| RepoError::CorruptRow(format!("unknown price state {}", row.state)))?;
        let Some(tally) = out.get_mut(&row.price_book_entry_id) else {
            continue;
        };
        match state {
            PriceState::Approved => {
                let (ended, future) = (count(row.ended)?, count(row.future)?);
                let active = n.checked_sub(ended + future).ok_or_else(|| {
                    RepoError::CorruptRow(format!(
                        "{ended} ended and {future} future of {n} prices"
                    ))
                })?;
                tally.prices.approved += n;
                tally.prices.superseded += ended;
                tally.prices.scheduled += future;
                tally.prices.active += active;
            }
            PriceState::Pending => tally.prices.pending += n,
            PriceState::Draft => tally.prices.draft += n,
            PriceState::Rejected | PriceState::Cancelled => {}
        }
    }
    let items = plan_item_repo::naming_entries(runner, &scope, tenant, entries).await?;
    let revision_ids: Vec<Uuid> = items
        .iter()
        .map(|i| i.revision_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut revisions = BTreeMap::new();
    for r in plan_revision_repo::find_many(runner, &scope, tenant, &revision_ids).await? {
        let state: RevisionState = r.state.parse().map_err(|_| {
            RepoError::CorruptRow(format!("unknown revision state {} of {}", r.state, r.id))
        })?;
        revisions.insert(r.id, (r.plan_id, state));
    }
    for item in items {
        let Some(entry) = item.price_book_entry_id else {
            continue;
        };
        let (plan, state) = revisions.get(&item.revision_id).ok_or_else(|| {
            RepoError::CorruptRow(format!(
                "plan item names lost revision {}",
                item.revision_id
            ))
        })?;
        let Some(tally) = out.get_mut(&entry) else {
            continue;
        };
        if *state == RevisionState::Superseded {
            tally.superseded.insert(*plan);
        } else {
            tally.live.insert(*plan);
        }
    }
    Ok(out)
}
/// The usage of each of the tenant's `entries` (D-428), its approved prices dated on `today`
/// (D-440), in a fixed number of statements; an entry nothing uses reads zeros.
/// # Errors
/// Storage failures; a stored state pricing does not know, or an item whose revision is gone, is a
/// corrupt row.
pub async fn entry_usage(
    runner: &impl DBRunner,
    tenant: Uuid,
    entries: &[Uuid],
    today: time::Date,
) -> Result<BTreeMap<Uuid, EntryUsage>, RepoError> {
    Ok(tallies(runner, tenant, entries, today)
        .await?
        .into_iter()
        .map(|(id, t)| {
            let usage = EntryUsage {
                prices: t.prices,
                plans: size(t.live.iter()),
                plans_superseded_only: size(t.superseded.difference(&t.live)),
            };
            (id, usage)
        })
        .collect())
}
/// Everything one SKU is counted from.
#[derive(Default)]
struct SkuTally {
    entries: u64,
    currencies: BTreeSet<String>,
    prices: PriceCounts,
    plans: BTreeSet<Uuid>,
}
/// The usage of each distinct SKU of `skus` in `tenant`, once, in the order first asked
/// (P-D-197). The SKUs' entries are read under the caller's `scope` — the aggregate the caller may
/// read — and every book, in every reference state; the rest tenant-scoped. `plans` is the union
/// of the entries' plans, never a sum of their counts. A SKU without an entry — unknown, another
/// tenant's, or a bundle — answers zeros.
/// # Errors
/// As [`entry_usage`]; an entry whose book is gone is a corrupt row.
pub async fn sku_usage(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
    skus: &[Uuid],
) -> Result<Vec<SkuUsage>, RepoError> {
    let mut seen = BTreeSet::new();
    let asked: Vec<Uuid> = skus.iter().copied().filter(|id| seen.insert(*id)).collect();
    let entries = price_book_entry_repo::for_skus(runner, scope, tenant, &asked).await?;
    let book_ids: Vec<Uuid> = entries
        .iter()
        .map(|e| e.book_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let currencies: BTreeMap<Uuid, String> =
        book_repo::find_many(runner, &AccessScope::for_tenant(tenant), tenant, &book_ids)
            .await?
            .into_iter()
            .map(|b| (b.id, b.currency))
            .collect();
    let ids: Vec<Uuid> = entries.iter().map(|e| e.id).collect();
    let tallies = tallies(runner, tenant, &ids, time::OffsetDateTime::now_utc().date()).await?;
    let mut by_sku: BTreeMap<Uuid, SkuTally> = BTreeMap::new();
    for e in &entries {
        let currency = currencies.get(&e.book_id).ok_or_else(|| {
            RepoError::CorruptRow(format!("entry {} names lost book {}", e.id, e.book_id))
        })?;
        let sku = by_sku.entry(e.sku_id).or_default();
        sku.entries += 1;
        sku.currencies.insert(currency.clone());
        if let Some(t) = tallies.get(&e.id) {
            sku.prices.approved += t.prices.approved;
            sku.prices.pending += t.prices.pending;
            sku.prices.draft += t.prices.draft;
            sku.plans.extend(t.live.iter().copied());
        }
    }
    Ok(asked
        .into_iter()
        .map(|sku_id| {
            let t = by_sku.remove(&sku_id).unwrap_or_default();
            SkuUsage {
                sku_id,
                entries: t.entries,
                currencies: t.currencies.into_iter().collect(),
                prices: t.prices,
                plans: size(t.plans.iter()),
            }
        })
        .collect())
}
/// The tenant's priced and in-plan SKUs (P-D-212): the SKUs whose [`sku_usage`] counts an entry,
/// and those whose [`sku_usage`] counts a plan, under the same `scope` — in two statements
/// whatever the number of SKUs.
/// # Errors
/// Storage failures.
pub async fn sku_usage_sets(
    runner: &impl DBRunner,
    scope: &AccessScope,
    tenant: Uuid,
) -> Result<SkuUsageSets, RepoError> {
    Ok(SkuUsageSets {
        priced: price_book_entry_repo::priced_skus(runner, scope, tenant).await?,
        in_plan: price_book_entry_repo::in_plan_skus(runner, scope, tenant).await?,
    })
}
