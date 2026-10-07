//! Plans: revisions bound to one book and their items, and the checks that decide whether a
//! revision may be submitted (D-394, D-407, D-408, D-413).
//!
//! A port of the prototype's `validatePlan`, `itemCoverage`, `planFrom` and `planBilling`
//! (`ui-prototype/pricebook/src/js/50-rules.js`). Everything here is plain data: the door reads each
//! item's SKU fresh through `sku_for_write` (D-408) and every entry with its prices, and hands the
//! lot in. The sold-as bundle and grants (D-411) and retirement (D-410) are deferred by the owner,
//! so neither `BUNDLE_SKU` nor `PLAN_RETIRING` is a check yet.
//!
//! @cpt-dod:cpt-cf-bss-pricing-dod-plan-item-rules:p1
//! @cpt-dod:cpt-cf-bss-pricing-dod-plan-coverage:p1
//! @cpt-dod:cpt-cf-bss-pricing-dod-plan-blocked-by:p1
use super::{
    book::{self, Book},
    price::{self, Price},
    price_book_entry::{ChargeKind, ReferenceState as EntryReferenceState, validate_entry_kind},
};
use bss_products_sdk::models::{Lifecycle, Sku, SkuType};
use std::collections::{BTreeMap, BTreeSet};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

// `scheduled`: approved, and waiting for its sale date (D-446).
string_enum!(RevisionState {Draft=>"draft", Pending=>"pending", Scheduled=>"scheduled", Published=>"published", Superseded=>"superseded"});
// The stored `treatment` column (D-467): no read shows it and no rule reads it. A row written from
// D-467 on is `paid` (`stored_treatment`); `optional` and `included` are legacy rows.
string_enum!(Treatment {Paid=>"paid", Optional=>"optional", Included=>"included"});
// A copied item starts `unreserved` and attaches after its write (D-413).
string_enum!(ReferenceState {Unreserved=>"unreserved", ConfirmationPending=>"confirmation_pending", Confirmed=>"confirmed", Lost=>"lost"});

/// The approval kind of a plan revision (spec §6); its quorum is the APPROVAL row's.
pub const KIND_PLAN_REVISION: &str = "plan_revision";
/// The most items one revision holds (`REVISION_ITEMS_TOO_MANY`).
pub const MAX_ITEMS: usize = 200;
/// The longest plan code a new plan takes (D-468).
pub const CODE_MAX: usize = 32;

/// Whether `code` is a plan code a new plan may take (D-468): `^[A-Z0-9][A-Z0-9_-]{0,31}$`, 1 to
/// [`CODE_MAX`] characters, upper-case ASCII letters, digits, `-` and `_`, starting with a letter
/// or a digit. It is judged as sent: no trim, no case folding. A code stored before the rule is
/// never judged again.
#[must_use]
pub fn code_follows_the_rule(code: &str) -> bool {
    let bytes = code.as_bytes();
    let first = bytes
        .first()
        .is_some_and(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
    first
        && bytes.len() <= CODE_MAX
        && bytes
            .iter()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
}

/// The plan's identity.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub id: Uuid,
    pub code: String,
    pub name: String,
}
/// The revision under check. `available_from` null means "at publish": the sale date is today.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revision {
    pub id: Uuid,
    pub rev_no: i32,
    pub book_id: Uuid,
    pub state: RevisionState,
    pub available_from: Option<Date>,
}
/// An item's Products reference: its state and the receipt a reserve answered, if any.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub state: ReferenceState,
    pub reservation_id: Option<Uuid>,
}
/// One plan item: a SKU and its entry in the plan's book (D-467). A null entry is a draft item
/// waiting for its entry (D-512), or a legacy row stored before D-467: the checks show it
/// `ITEM_ENTRY_MISSING`.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: Uuid,
    pub sku_id: Uuid,
    pub price_book_entry_id: Option<Uuid>,
    pub reference: Reference,
}
/// The `treatment` a row written from D-467 on stores: `paid` for an item that names an entry,
/// and `included` for an item with no entry (a draft waiting for one, D-512, or a copy of a
/// legacy item), the entry-less row the column's CHECK admits. The quantities it stores are
/// always null.
#[must_use]
pub const fn stored_treatment(entry: Option<Uuid>) -> Treatment {
    if entry.is_some() {
        Treatment::Paid
    } else {
        Treatment::Included
    }
}
/// A pending price and the approval unit that holds it: a candidate for `blocked_by`.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingPrice {
    pub price_id: Uuid,
    pub unit_id: Uuid,
}
/// An entry an item names, with its approved and pending prices and its reference state.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: Uuid,
    pub book_id: Uuid,
    pub sku_id: Uuid,
    pub charge_kind: ChargeKind,
    pub period: Option<String>,
    pub dimension_key: Option<String>,
    pub reference_state: EntryReferenceState,
    pub prices: Vec<Price>,
    pub pending: Vec<PendingPrice>,
}
/// A book the check reads: the revision's own, and any other book an item's entry lives in.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanBook {
    pub id: Uuid,
    pub book: Book,
}
/// The tenant's descriptor defaults, shown for information only (D-408).
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Defaults {
    pub gl: Option<String>,
    pub rounding: String,
    pub tax_category: Option<String>,
}
/// Everything the checks read, as plain data.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanContext {
    pub plan: Plan,
    pub revision: Revision,
    pub items: Vec<Item>,
    /// Each item's SKU, read fresh (D-408); a SKU missing here is unavailable.
    pub skus: Vec<Sku>,
    pub entries: Vec<Entry>,
    pub books: Vec<PlanBook>,
    /// Each registered dimension key with its values.
    pub dimension_values: Vec<(String, Vec<String>)>,
    /// The item SKUs of this plan's published revision, the one in effect on the checks' day
    /// (D-447): a deprecated SKU may be carried over from there, never added (D-408). Empty for a
    /// clone, which is a new plan.
    pub published_sku_ids: Vec<Uuid>,
    pub quorum: u32,
    pub defaults: Defaults,
}
/// An item a check row is about (D-466): the item, its SKU and the entry it names. The wire names
/// (`item_id`, ...) are the DTO's, `PricingPlanCheckSubject`; the domain names the aggregates (the
/// phase 9 review's R55).
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Subject {
    pub item: Uuid,
    pub sku: Uuid,
    pub entry: Option<Uuid>,
}
/// A pending price that blocks a check row (D-466): its approval unit, the price and its entry.
/// Ordered by unit, then price, as [`Check::blocked_by`] orders its units. The wire names are the
/// DTO's, `PricingPlanCheckBlockingPrice` (the phase 9 review's R56).
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct BlockingPrice {
    pub unit: Uuid,
    pub price: Uuid,
    pub entry: Uuid,
}
/// The distinct units of `prices`, in their order: `prices` is ordered by unit, so each unit's
/// prices are adjacent.
fn units_of(prices: &[BlockingPrice]) -> Vec<Uuid> {
    let mut units: Vec<Uuid> = prices.iter().map(|p| p.unit).collect();
    units.dedup();
    units
}
/// One check row. An `info` row is always ok and never blocks.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub code: &'static str,
    pub ok: bool,
    pub label: String,
    pub detail: String,
    pub info: bool,
    /// The items that turn the row red, in the revision's item order (D-466): empty for a green
    /// row and for a plan-wide one.
    pub subjects: Vec<Subject>,
    /// The pending prices that would cover what is uncovered, one per price, ordered by unit then
    /// price (D-466); computed, never stored (spec §6). [`Check::blocked_by`] derives their units.
    pub blocked_by_prices: Vec<BlockingPrice>,
}
impl Check {
    /// The approval units whose pending prices would cover what is uncovered: the distinct units
    /// of `blocked_by_prices`, in order. Derived, so the two cannot drift (the phase 9 review's
    /// R19).
    #[must_use]
    pub fn blocked_by(&self) -> Vec<Uuid> {
        units_of(&self.blocked_by_prices)
    }
}
/// Whether one item is priced on the sale date, in the plan's book.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemCoverage {
    pub ok: bool,
    pub detail: String,
    pub version_no: Option<i32>,
    /// The pending prices that would cover the item (D-466), ordered by unit then price.
    pub blocked_by_prices: Vec<BlockingPrice>,
}
impl ItemCoverage {
    /// The distinct units of `blocked_by_prices`, in order (the phase 9 review's R20).
    #[must_use]
    pub fn blocked_by(&self) -> Vec<Uuid> {
        units_of(&self.blocked_by_prices)
    }
}

/// A stored revision as [`effective`] reads it: the columns its effective state depends on.
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredRevision {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub rev_no: i32,
    pub state: RevisionState,
    pub available_from: Option<Date>,
    pub published_at: Option<OffsetDateTime>,
}
/// What a stored revision reads as on one day (D-447).
#[toolkit_macros::domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectiveRevision {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub rev_no: i32,
    pub state: RevisionState,
    pub published_at: Option<OffsetDateTime>,
}
/// Whether a stored revision is a scheduled one whose sale date has come: `available_from` on or
/// before `today`. A scheduled revision without a date is never due; the storage writes none (the
/// apply schedules only a future date), and `plan_revision_repo::switch_due` reads it the same way.
#[must_use]
pub fn is_due(revision: &StoredRevision, today: Date) -> bool {
    revision.state == RevisionState::Scheduled
        && revision.available_from.is_some_and(|from| from <= today)
}
/// The instant a due revision reads as published at: 00:00 UTC of its sale date.
#[must_use]
pub fn published_from(from: Date) -> OffsetDateTime {
    from.midnight().assume_utc()
}
/// Every revision as it reads on `today` (D-447), in the order given; a pure function of the stored
/// rows, so a read never writes. A due scheduled revision (see [`is_due`]) reads `published`, with
/// `published_at` from [`published_from`]; the stored-published revision of its plan reads
/// `superseded`, keeping its own `published_at`; every other revision reads as stored. Plans are told
/// apart by `plan_id`, so one list may carry the revisions of many plans.
#[must_use]
pub fn effective(revisions: &[StoredRevision], today: Date) -> Vec<EffectiveRevision> {
    let switching: BTreeSet<Uuid> = revisions
        .iter()
        .filter(|r| is_due(r, today))
        .map(|r| r.plan_id)
        .collect();
    revisions
        .iter()
        .map(|r| {
            let (state, published_at) = if is_due(r, today) {
                (
                    RevisionState::Published,
                    r.available_from.map(published_from),
                )
            } else if r.state == RevisionState::Published && switching.contains(&r.plan_id) {
                (RevisionState::Superseded, r.published_at)
            } else {
                (r.state, r.published_at)
            };
            EffectiveRevision {
                id: r.id,
                plan_id: r.plan_id,
                rev_no: r.rev_no,
                state,
                published_at,
            }
        })
        .collect()
}
/// The revision a plan's list row names as its current one (D-460), among ONE plan's revisions as
/// they read on a day ([`effective`]): the draft or pending one (a plan holds at most one, D-451),
/// else the scheduled one still waiting for its date, else the published one in effect. A due
/// scheduled revision reads published, so it is current once its date has come. `None` for a plan
/// without revisions.
#[must_use]
pub fn current(revisions: &[EffectiveRevision]) -> Option<&EffectiveRevision> {
    let first = |states: &[RevisionState]| revisions.iter().find(|r| states.contains(&r.state));
    first(&[RevisionState::Draft, RevisionState::Pending])
        .or_else(|| first(&[RevisionState::Scheduled]))
        .or_else(|| in_effect(revisions))
}
/// The published revision in effect among ONE plan's revisions as they read on a day
/// ([`effective`], D-447, D-460): the one the plan sells today; `None` before its first
/// publication.
#[must_use]
pub fn in_effect(revisions: &[EffectiveRevision]) -> Option<&EffectiveRevision> {
    revisions
        .iter()
        .find(|r| r.state == RevisionState::Published)
}
/// The plan's `published_rev` as it reads on `today` (D-447): the number of its due scheduled
/// revision when it has one, else `stored`, the plan's own projection. Only the due revision need be
/// among `revisions`.
#[must_use]
pub fn published_rev(
    stored: Option<i32>,
    revisions: &[StoredRevision],
    plan_id: Uuid,
    today: Date,
) -> Option<i32> {
    revisions
        .iter()
        .find(|r| r.plan_id == plan_id && is_due(r, today))
        .map_or(stored, |r| Some(r.rev_no))
}

/// The sale date: `available_from`, or today for "at publish" (the prototype's `planFrom`).
#[must_use]
pub fn sale_date(revision: &Revision, today: Date) -> Date {
    revision.available_from.unwrap_or(today)
}
/// The revision's book, if the context carries it.
#[must_use]
pub fn book(ctx: &PlanContext) -> Option<&PlanBook> {
    book_by_id(ctx, ctx.revision.book_id)
}
/// The currency the plan sells in: its book's.
#[must_use]
pub fn currency(ctx: &PlanContext) -> Option<&str> {
    book(ctx).map(|b| b.book.currency.as_str())
}
/// The recurring periods the plan's items bill in (the prototype's `planBilling`).
#[must_use]
pub fn billing(ctx: &PlanContext) -> Vec<String> {
    ctx.items
        .iter()
        .filter_map(|it| entry(ctx, it.price_book_entry_id))
        .filter(|e| e.charge_kind == ChargeKind::Recurring)
        .map(|e| e.period.clone().unwrap_or_else(|| "-".to_owned()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
/// Whether every check is ok.
#[must_use]
pub fn ready(checks: &[Check]) -> bool {
    checks.iter().all(|c| c.ok)
}

fn book_by_id(ctx: &PlanContext, id: Uuid) -> Option<&PlanBook> {
    ctx.books.iter().find(|b| b.id == id)
}
fn sku_of(ctx: &PlanContext, id: Uuid) -> Option<&Sku> {
    ctx.skus.iter().find(|s| s.id == id)
}
fn entry(ctx: &PlanContext, id: Option<Uuid>) -> Option<&Entry> {
    id.and_then(|id| ctx.entries.iter().find(|e| e.id == id))
}
fn name_of(ctx: &PlanContext, item: &Item) -> String {
    sku_of(ctx, item.sku_id).map_or_else(|| item.sku_id.to_string(), |s| s.name.clone())
}
fn values_of<'a>(ctx: &'a PlanContext, e: &Entry) -> &'a [String] {
    e.dimension_key
        .as_ref()
        .and_then(|key| ctx.dimension_values.iter().find(|(k, _)| k == key))
        .map_or(&[], |(_, values)| values.as_slice())
}
/// Every pending price of the entry, with the approval unit that holds it: the default chain can
/// cover a value, so a pending default price blocks it as much as the value's own. Answers the
/// prices, distinct and ordered by unit then price; their units are `blocked_by` (D-466).
fn pending_units(e: &Entry) -> Vec<BlockingPrice> {
    // @cpt-begin:cpt-cf-bss-pricing-algo-plans-revision-checks:p1:inst-plans-revision-checks-4
    let prices: BTreeSet<BlockingPrice> = e
        .pending
        .iter()
        .map(|p| BlockingPrice {
            unit: p.unit_id,
            price: p.price_id,
            entry: e.id,
        })
        .collect();
    prices.into_iter().collect()
    // @cpt-end:cpt-cf-bss-pricing-algo-plans-revision-checks:p1:inst-plans-revision-checks-4
}
fn uncovered(detail: String, blocking: Vec<BlockingPrice>) -> ItemCoverage {
    ItemCoverage {
        ok: false,
        detail,
        version_no: None,
        blocked_by_prices: blocking,
    }
}

/// Coverage of one item on the sale date: per dimension value, through its own chain or the
/// default, with an open tail (the prototype's `itemCoverage`).
#[must_use]
pub fn item_coverage(ctx: &PlanContext, item: &Item, today: Date) -> ItemCoverage {
    let Some(e) = entry(ctx, item.price_book_entry_id) else {
        return uncovered("no price".into(), vec![]);
    };
    let Some(b) = book(ctx) else {
        return uncovered("attach a price book".into(), vec![]);
    };
    if e.book_id != b.id {
        let other = book_by_id(ctx, e.book_id).map_or("another book", |o| o.book.name.as_str());
        return uncovered(format!("priced in {other}, not {}", b.book.name), vec![]);
    }
    let date = sale_date(&ctx.revision, today);
    let values = values_of(ctx, e);
    let key = e.dimension_key.as_deref().unwrap_or_default();
    // @cpt-begin:cpt-cf-bss-pricing-algo-plans-revision-checks:p1:inst-plans-revision-checks-3
    let cov = price::coverage_on(&e.prices, e.id, date, values);
    if !cov.missing.is_empty() {
        let detail = if values.is_empty() {
            format!("no approved price on {date}")
        } else {
            format!(
                "{key} {}: no price on {date} and no default price",
                cov.missing.join(", ")
            )
        };
        return uncovered(detail, pending_units(e));
    }
    if !cov.closing.is_empty() {
        let detail = if values.is_empty() {
            let end = price::approved_prices(&e.prices, e.id, None)
                .last()
                .and_then(|p| p.effective_to)
                .map_or_else(|| "?".to_owned(), |d| d.to_string());
            format!("last window closes {end}")
        } else {
            format!(
                "{key} {}: last window closes and no default carries on",
                cov.closing.join(", ")
            )
        };
        return uncovered(detail, pending_units(e));
    }
    // @cpt-end:cpt-cf-bss-pricing-algo-plans-revision-checks:p1:inst-plans-revision-checks-3
    let version_no = cov.version.map(|p| p.version_no);
    let dims = if values.is_empty() {
        String::new()
    } else {
        format!(" \u{b7} {} \u{d7} {key}", values.len())
    };
    ItemCoverage {
        ok: true,
        detail: format!(
            "{} \u{2713} v{}{dims}",
            b.book.currency,
            version_no.unwrap_or_default()
        ),
        version_no,
        blocked_by_prices: vec![],
    }
}

/// What one check found: the lines its detail lists, and the items they name (D-466).
#[derive(Default)]
struct Found {
    lines: Vec<String>,
    items: BTreeSet<Uuid>,
}
impl Found {
    fn add(&mut self, line: impl Into<String>, items: &[Uuid]) {
        self.lines.push(line.into());
        self.items.extend(items.iter().copied());
    }
    fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}
/// What the item walk found, one list per check.
#[derive(Default)]
struct Tally {
    no_entry: Found,
    mismatch: Found,
    entry_lost: Found,
    bundle: Found,
    kind_clash: Found,
    foreign: Found,
    uncovered: Found,
    blocking: BTreeSet<BlockingPrice>,
    /// Each recurring period with the items that bill in it.
    periods: BTreeMap<String, Vec<Uuid>>,
    /// Each metered usage type with the first item that meters it.
    meters: Vec<(String, String, Uuid)>,
    meter_dup: Found,
    deprecated: Found,
    unavailable: Found,
    pending: Found,
    lost: Found,
}

/// Lifecycle and reference, for every item: a fresh SKU read and a receipt (D-408, D-413).
fn tally_sku_and_reference(ctx: &PlanContext, item: &Item, name: &str, t: &mut Tally) {
    // @cpt-begin:cpt-cf-bss-pricing-algo-plans-revision-checks:p1:inst-plans-revision-checks-1
    let it = [item.id];
    match sku_of(ctx, item.sku_id) {
        None => t.unavailable.add(format!("{name} - not found"), &it),
        Some(sku) if sku.retire_pending => t.unavailable.add(name, &it),
        Some(sku) => match sku.lifecycle {
            Lifecycle::Draft | Lifecycle::Retired => t.unavailable.add(name, &it),
            Lifecycle::Deprecated if !ctx.published_sku_ids.contains(&item.sku_id) => {
                t.deprecated.add(name, &it);
            }
            Lifecycle::Deprecated | Lifecycle::Published => {}
        },
    }
    match item.reference.state {
        ReferenceState::Confirmed => {}
        ReferenceState::ConfirmationPending if item.reference.reservation_id.is_some() => {}
        ReferenceState::Lost => t.lost.add(name, &it),
        ReferenceState::Unreserved | ReferenceState::ConfirmationPending => {
            t.pending.add(name, &it);
        }
    }
    // @cpt-end:cpt-cf-bss-pricing-algo-plans-revision-checks:p1:inst-plans-revision-checks-1
}
/// Structure (the prototype's walk up to its entry): charge kind and meter. An item without an
/// entry meters nothing: it is `ITEM_ENTRY_MISSING` (D-467).
fn tally_structure(sku: &Sku, e: Option<&Entry>, item: &Item, name: &str, t: &mut Tally) {
    // @cpt-begin:cpt-cf-bss-pricing-algo-plans-revision-checks:p1:inst-plans-revision-checks-1
    if let Some(e) = e
        && validate_entry_kind(e.charge_kind, sku.r#type).is_err()
    {
        t.kind_clash.add(
            format!(
                "{name} - entry is {}, SKU is {}",
                e.charge_kind.as_str(),
                sku.r#type.as_str()
            ),
            &[item.id],
        );
    }
    // @cpt-end:cpt-cf-bss-pricing-algo-plans-revision-checks:p1:inst-plans-revision-checks-1
    // @cpt-begin:cpt-cf-bss-pricing-algo-plans-revision-checks:p1:inst-plans-revision-checks-2
    if let Some(meter) = &sku.usage_type_ref
        && e.is_some()
    {
        if let Some((_, first, first_item)) = t.meters.iter().find(|(m, _, _)| m == meter) {
            t.meter_dup
                .add(format!("{name} <-> {first}"), &[*first_item, item.id]);
        } else {
            t.meters.push((meter.clone(), name.to_owned(), item.id));
        }
    }
    // @cpt-end:cpt-cf-bss-pricing-algo-plans-revision-checks:p1:inst-plans-revision-checks-2
}
/// Pricing: the entry, its book and its coverage on the sale date.
fn tally_pricing(
    ctx: &PlanContext,
    e: &Entry,
    item: &Item,
    name: &str,
    t: &mut Tally,
    today: Date,
) {
    let it = [item.id];
    if e.sku_id != item.sku_id {
        t.mismatch
            .add(format!("{name} - the entry prices another SKU"), &it);
    }
    // D-522: a released entry (its book archived) holds no reference either.
    if matches!(
        e.reference_state,
        EntryReferenceState::Lost | EntryReferenceState::Released
    ) {
        t.entry_lost.add(name, &it);
    }
    // @cpt-begin:cpt-cf-bss-pricing-algo-plans-revision-checks:p1:inst-plans-revision-checks-2
    if e.book_id != ctx.revision.book_id {
        let describe = |id: Uuid| {
            book_by_id(ctx, id).map_or_else(
                || "? (?)".to_owned(),
                |b| format!("{} ({})", b.book.name, b.book.currency),
            )
        };
        t.foreign.add(
            format!(
                "{name} - priced in {}, the plan reads {}",
                describe(e.book_id),
                describe(ctx.revision.book_id)
            ),
            &it,
        );
        return;
    }
    if e.charge_kind == ChargeKind::Recurring {
        t.periods
            .entry(e.period.clone().unwrap_or_else(|| "-".to_owned()))
            .or_default()
            .push(item.id);
    }
    // @cpt-end:cpt-cf-bss-pricing-algo-plans-revision-checks:p1:inst-plans-revision-checks-2
    let cov = item_coverage(ctx, item, today);
    if !cov.ok {
        t.uncovered.add(format!("{name} - {}", cov.detail), &it);
        t.blocking.extend(cov.blocked_by_prices);
    }
}
fn tally(ctx: &PlanContext, today: Date) -> Tally {
    let mut t = Tally::default();
    for item in &ctx.items {
        let name = name_of(ctx, item);
        tally_sku_and_reference(ctx, item, &name, &mut t);
        let e = entry(ctx, item.price_book_entry_id);
        if let Some(sku) = sku_of(ctx, item.sku_id) {
            // @cpt-begin:cpt-cf-bss-pricing-algo-plans-revision-checks:p1:inst-plans-revision-checks-1
            if sku.r#type == SkuType::Bundle {
                t.bundle.add(name, &[item.id]);
                continue;
            }
            // @cpt-end:cpt-cf-bss-pricing-algo-plans-revision-checks:p1:inst-plans-revision-checks-1
            tally_structure(sku, e, item, &name, &mut t);
        }
        match e {
            Some(e) => tally_pricing(ctx, e, item, &name, &mut t, today),
            None => t.no_entry.add(name, &[item.id]),
        }
    }
    t
}

fn row(code: &'static str, ok: bool, label: impl Into<String>, detail: impl Into<String>) -> Check {
    Check {
        code,
        ok,
        label: label.into(),
        detail: detail.into(),
        info: false,
        subjects: vec![],
        blocked_by_prices: vec![],
    }
}
/// The items of `ids`, in the revision's item order (D-466).
fn subjects(ctx: &PlanContext, ids: &BTreeSet<Uuid>) -> Vec<Subject> {
    ctx.items
        .iter()
        .filter(|it| ids.contains(&it.id))
        .map(|it| Subject {
            item: it.id,
            sku: it.sku_id,
            entry: it.price_book_entry_id,
        })
        .collect()
}
/// A row of what the walk `found`: red while it found anything, its detail the lines found (or
/// `otherwise`), its subjects the items they name.
fn found_row(
    ctx: &PlanContext,
    code: &'static str,
    label: impl Into<String>,
    found: &Found,
    otherwise: &str,
) -> Check {
    Check {
        subjects: subjects(ctx, &found.items),
        ..row(
            code,
            found.is_empty(),
            label,
            listed(&found.lines, otherwise),
        )
    }
}
fn listed(found: &[String], otherwise: &str) -> String {
    if found.is_empty() {
        otherwise.to_owned()
    } else {
        found.join("; ")
    }
}
fn plan_rows(ctx: &PlanContext, sale: Date) -> Vec<Check> {
    let name = ctx.plan.name.trim();
    let mut rows = vec![row(
        "PLAN_NAME",
        !name.is_empty(),
        "Plan has a name",
        if name.is_empty() {
            "name is required"
        } else {
            name
        },
    )];
    let b = book(ctx);
    rows.push(row(
        "PLAN_BOOK",
        b.is_some(),
        "Exactly one price book attached",
        b.map_or_else(
            || {
                "a plan reads one book and sells in its currency; another currency is another plan"
                    .to_owned()
            },
            |b| format!("{} -> sells in {}", b.book.name, b.book.currency),
        ),
    ));
    if let Some(b) = b
        && (b.book.valid_from.is_some() || b.book.valid_until.is_some())
    {
        // @cpt-begin:cpt-cf-bss-pricing-algo-plans-revision-checks:p1:inst-plans-revision-checks-3
        let valid = book::valid_on(&b.book, sale);
        // @cpt-end:cpt-cf-bss-pricing-algo-plans-revision-checks:p1:inst-plans-revision-checks-3
        let window = format!(
            "{} -> {}",
            b.book
                .valid_from
                .map_or_else(|| "...".to_owned(), |d| d.to_string()),
            b.book
                .valid_until
                .map_or_else(|| "open".to_owned(), |d| d.to_string())
        );
        let detail = if valid {
            format!("{} valid {window}", b.book.name)
        } else {
            format!("{} is NOT valid on {sale} - {window}", b.book.name)
        };
        rows.push(row(
            "PLAN_BOOK_VALIDITY",
            valid,
            "The book is valid on the sale date",
            detail,
        ));
    }
    rows.push(row(
        "PLAN_ITEMS",
        !ctx.items.is_empty(),
        "At least one item",
        format!("{} item(s)", ctx.items.len()),
    ));
    rows
}
fn entry_rows(ctx: &PlanContext, t: &Tally, sale: Date) -> Vec<Check> {
    let currency = currency(ctx).unwrap_or("no book");
    let mut uncovered = found_row(
        ctx,
        "ITEM_UNCOVERED",
        format!("Every item has an approved, open-ended price from {sale}"),
        &t.uncovered,
        &format!("covered in {currency}"),
    );
    uncovered.blocked_by_prices = t.blocking.iter().copied().collect();
    vec![
        found_row(
            ctx,
            "ITEM_ENTRY_MISSING",
            "Every item points at a price",
            &t.no_entry,
            "all items priced",
        ),
        found_row(
            ctx,
            "ITEM_ENTRY_SKU_MISMATCH",
            "Every item's entry prices that item's SKU",
            &t.mismatch,
            "ok",
        ),
        found_row(
            ctx,
            "ITEM_ENTRY_LOST",
            "No item's entry has lost its Products reference",
            &t.entry_lost,
            "ok",
        ),
        found_row(
            ctx,
            "ITEM_BUNDLE_SKU",
            "No bundle SKU sits inside the plan as an item",
            &t.bundle,
            "ok",
        ),
        found_row(
            ctx,
            "CHARGE_KIND_SKU_TYPE",
            "Every item charges the way its SKU is typed",
            &t.kind_clash,
            "ok",
        ),
        found_row(
            ctx,
            "ITEM_BOOK_FOREIGN",
            "Every item is priced in the plan's book",
            &t.foreign,
            "all from the plan's book",
        ),
        uncovered,
    ]
}
fn structure_rows(ctx: &PlanContext, t: &Tally) -> Vec<Check> {
    let periods: Vec<_> = t.periods.keys().cloned().collect();
    let frequency = match periods.as_slice() {
        [] => "no recurring items".to_owned(),
        [one] => format!("billed every {one}"),
        many => format!("found {} - one period per plan", many.join(" and ")),
    };
    // Mixed periods name every recurring item priced in the plan's book: each bills in one of them.
    let mixed: BTreeSet<Uuid> = if periods.len() > 1 {
        t.periods.values().flatten().copied().collect()
    } else {
        BTreeSet::new()
    };
    vec![
        Check {
            subjects: subjects(ctx, &mixed),
            ..row(
                "FREQUENCY_MIXED",
                periods.len() <= 1,
                "All recurring items share one billing period",
                frequency,
            )
        },
        found_row(
            ctx,
            "METER_DUPLICATE",
            "No two items meter the same usage type",
            &t.meter_dup,
            "meters unambiguous",
        ),
    ]
}
fn sku_rows(ctx: &PlanContext, t: &Tally) -> Vec<Check> {
    vec![
        found_row(
            ctx,
            "ITEM_SKU_DEPRECATED",
            "No deprecated SKU enters the plan",
            &t.deprecated,
            "a deprecated SKU only stays when carried over within the same plan",
        ),
        found_row(
            ctx,
            "ITEM_SKU_UNAVAILABLE",
            "Every item SKU is published or deprecated",
            &t.unavailable,
            "ok",
        ),
        found_row(
            ctx,
            "ITEM_REFERENCE_PENDING",
            "Every item reference holds a Products receipt",
            &t.pending,
            "ok",
        ),
        found_row(
            ctx,
            "ITEM_REFERENCE_LOST",
            "No item reference is lost",
            &t.lost,
            "ok",
        ),
    ]
}
fn info_rows(ctx: &PlanContext) -> Vec<Check> {
    let d = &ctx.defaults;
    let descriptors = format!(
        "invoice line: entry override -> SKU -> tenant default; GL: SKU -> {}; rounding {}; tax {}",
        d.gl.as_deref().unwrap_or("none"),
        d.rounding,
        d.tax_category.as_deref().unwrap_or("none")
    );
    let approval = if ctx.quorum == 0 {
        "publishes directly - no second person".to_owned()
    } else {
        format!("needs {} independent approver(s)", ctx.quorum)
    };
    [
        row(
            "DESCRIPTORS",
            true,
            "Invoice line, GL code, tax, rounding resolved",
            descriptors,
        ),
        row(
            "APPROVAL",
            true,
            format!("Approval quorum {}", ctx.quorum),
            approval,
        ),
    ]
    .into_iter()
    .map(|c| Check { info: true, ..c })
    .collect()
}

/// Every check of a revision on its sale date (the prototype's `validatePlan`), in a fixed order.
#[must_use]
pub fn checks(ctx: &PlanContext, today: Date) -> Vec<Check> {
    let sale = sale_date(&ctx.revision, today);
    let t = tally(ctx, today);
    let mut out = plan_rows(ctx, sale);
    out.extend(entry_rows(ctx, &t, sale));
    out.extend(structure_rows(ctx, &t));
    out.extend(sku_rows(ctx, &t));
    out.extend(info_rows(ctx));
    out
}
#[cfg(test)]
#[path = "plan_effective_tests.rs"]
mod effective_tests;
#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
