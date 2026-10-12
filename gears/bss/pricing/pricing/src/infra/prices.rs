//! The `prices` approval subject (spec §6): draft prices of one book, optionally moved to a
//! common effective date, approved together inside the caller's transaction.
//!
//! No price is locked by the database: ownership is the conditional `pending_unit_id` write,
//! and `apply` re-reads every touched chain in the door's serializable transaction.
//!
//! @cpt-dod:cpt-cf-bss-pricing-dod-chain-windows:p1
//! @cpt-dod:cpt-cf-bss-pricing-dod-pair-guard:p1
//! @cpt-dod:cpt-cf-bss-pricing-dod-prices-unit:p1
//! @cpt-dod:cpt-cf-bss-pricing-dod-sod-excludes-authors:p1
use crate::{
    domain::{
        RuleError, book,
        price::{self, ChangeKind, Eligibility, Price, PriceState, SkuMetering},
        price_book_entry::{ChargeKind, Model},
    },
    infra::{
        plan_revisions::{self, SUBSCRIPTIONS_UNAVAILABLE},
        reference_work,
        storage::{
            RepoError,
            entity::{self, price_book_entry},
            repo::{
                acceptance_repo, book_repo, dimension_repo, plan_item_repo, plan_repo,
                plan_revision_repo, price_book_entry_repo, price_repo,
            },
        },
    },
};
use bss_approval::{ApprovalError, ApprovalSubject, ItemRef, Unit};

use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, PoisonError},
};
use time::{Date, OffsetDateTime};
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::{
    DbTx,
    secure::{AccessScope, DBRunner},
};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// The approval kind of a batch of book prices.
pub const KIND_PRICES: &str = "prices";
/// A `prices` unit references its book.
pub const REF_TYPE: &str = "price_book";
/// Every item of a `prices` unit is one price.
pub const ITEM_TYPE: &str = "price";

/// What the pure rules need to judge a price of one entry.
pub struct PriceBookEntryContext {
    pub kind: ChargeKind,
    /// The entry's model (D-427): every price of the entry is decoded and judged in it.
    pub model: Model,
    pub values: Option<Vec<String>>,
    pub digits: u32,
    pub prices: Vec<entity::price::Model>,
    pub policy: Option<crate::infra::usage_policy_wire::UsageRatingPolicy>,
}
impl PriceBookEntryContext {
    /// Read the book currency, the declared dimension values and every price of the entry.
    /// # Errors
    /// Returns storage failures or a corrupt stored vocabulary.
    pub async fn load(
        tx: &impl DBRunner,
        tenant: Uuid,
        entry: &price_book_entry::Model,
    ) -> Result<Self, RepoError> {
        let children = AccessScope::for_tenant(tenant);
        let kind = entry
            .charge_kind
            .parse()
            .map_err(|_| RepoError::CorruptRow(format!("entry {} charge_kind", entry.id)))?;
        let book = book_repo::find(tx, &children, tenant, entry.book_id)
            .await?
            .ok_or_else(|| RepoError::CorruptRow(format!("entry {} has no book", entry.id)))?;
        let values = match &entry.dimension_key {
            Some(key) => dimension_repo::find(tx, &children, tenant, key)
                .await?
                .map(|d| {
                    serde_json::from_value::<Vec<String>>(d.values)
                        .map_err(|_| RepoError::CorruptRow(format!("dimension {key} values")))
                })
                .transpose()?,
            None => None,
        };
        let prices = price_repo::for_entry(tx, &children, tenant, entry.id).await?;
        Ok(Self {
            kind,
            model: price_book_entry_repo::model_of(entry)?,
            values,
            digits: book::minor_digits(&book.currency),
            prices,
            policy: crate::infra::storage::repo::usage_policy_repo::for_entries(
                tx,
                tenant,
                std::slice::from_ref(entry),
            )
            .await?
            .remove(&entry.id),
        })
    }
    /// Every price of the entry in the pure model. A `cancel` or `end` row is not a price, so it
    /// is not here (D-520, D-521); `prices` keeps every row.
    /// # Errors
    /// Returns a corrupt stored price.
    pub fn domain_prices(&self) -> Result<Vec<Price>, RepoError> {
        self.prices
            .iter()
            .filter(|m| price_repo::is_price(m))
            .map(|m| price_repo::to_domain(m, self.model))
            .collect()
    }
    /// The first pure refusal of a candidate against the entry's approved prices.
    #[must_use]
    pub fn first_refusal(
        &self,
        candidate: &Price,
        siblings: &[Price],
        today: Date,
    ) -> Option<RuleError> {
        if candidate.min_fee.is_some()
            && self.policy.as_ref().is_some_and(|p| {
                crate::domain::usage_policy::refuses_minimum_fee(&(&p.content).into())
            })
        {
            return Some(RuleError::new("UNSUPPORTED_TERMS"));
        }
        price::validate(
            candidate,
            self.kind,
            self.values.as_deref(),
            siblings,
            today,
            self.digits,
        )
        .first()
        .copied()
    }
}

/// What rejecting or withdrawing leaves on the unit's prices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Release {
    /// Withdraw: the prices are editable drafts again.
    Draft,
    /// Reject: the prices keep their review history and stay rejected.
    Rejected,
}

/// The subject of one `prices` unit of one book.
#[derive(Clone)]
pub struct PricesSubject {
    pub meter_observations: crate::infra::meter_semantics::Observations,
    /// The caller; dated SKU reads are made on its behalf.
    pub ctx: SecurityContext,
    pub hub: Arc<toolkit::ClientHub>,
    pub tenant_id: Uuid,
    pub book_id: Uuid,
    pub now: OffsetDateTime,
    /// The unit's shared start; the door copies it from the unit when voting.
    pub common_effective_date: Option<Date>,
    /// Partners that publish-changes pulled in; recorded in the snapshot.
    pub added_partner: Vec<Uuid>,
    pub release: Release,
    /// A definite Products refusal met while judging, kept whole for the door: the approval
    /// error carries only static codes, and the caller must see Products' own status and code.
    refused: Arc<Mutex<Option<CanonicalError>>>,
    /// What `collect` gathers for the synchronous `snapshot` (D-408): the plans reading the
    /// unit's entries and each entry SKU's current descriptors, both outside `after`.
    review: Arc<Mutex<Review>>,
    /// The approved prices outside the unit whose window or state the last `apply` changed: the
    /// predecessors it re-closed or re-opened, and the prices it cancelled or ended. The event
    /// lists them with the unit's prices (D-520, D-521).
    moved: Arc<Mutex<BTreeSet<Uuid>>>,
}

/// The reviewer's information about a unit that is never fingerprinted content.
#[derive(Default)]
struct Review {
    plans: Vec<Value>,
    /// The entry SKUs' descriptors, or `"unavailable"` when Products did not answer the read.
    descriptors: Value,
}

/// One entry's part of a unit, judged against the entry's current approved prices.
struct Judged {
    /// Every row of the entry, as stored.
    stored: Vec<entity::price::Model>,
    /// The unit's prices as they will be approved: shifted, not yet normalised.
    proposed: Vec<Price>,
    /// Every approved price of the entry once the unit applies, normalised per chain: a price the
    /// unit cancels is gone, and a price it ends carries its explicit end.
    chain: Vec<Price>,
    /// The unit's `cancel` and `end` rows (D-520, D-521).
    changes: Vec<Change>,
    /// The chains the unit touches: its prices' and the changed prices' dimension values.
    touched: BTreeSet<Option<String>>,
}

/// A `cancel` or `end` row as its guards read it (D-520, D-521). A price (`set`) is not a change,
/// and only an end carries a new end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// Cancel the approved price `target`. `id` is the change row; nil at the door that has not
    /// written it yet.
    Cancel { id: Uuid, target: Uuid },
    /// End the approved price `target` at `end`, exclusive. `id` as for a cancel.
    End { id: Uuid, target: Uuid, end: Date },
}
impl Change {
    /// The change row; nil at the door that has not written it yet.
    #[must_use]
    pub const fn id(&self) -> Uuid {
        match *self {
            Self::Cancel { id, .. } | Self::End { id, .. } => id,
        }
    }
    /// The approved price the change names.
    #[must_use]
    pub const fn target(&self) -> Uuid {
        match *self {
            Self::Cancel { target, .. } | Self::End { target, .. } => target,
        }
    }
    /// The stored `change_kind` of the row.
    #[must_use]
    pub const fn kind(&self) -> ChangeKind {
        match self {
            Self::Cancel { .. } => ChangeKind::Cancel,
            Self::End { .. } => ChangeKind::End,
        }
    }
    /// An end's new end; `None` for a cancel.
    #[must_use]
    pub const fn end(&self) -> Option<Date> {
        match *self {
            Self::Cancel { .. } => None,
            Self::End { end, .. } => Some(end),
        }
    }
    /// The change a stored row asks for; `None` for a price (`set`).
    /// # Errors
    /// A corrupt row: an unknown kind, a change that names no price, or an end with no new end.
    pub fn of(m: &entity::price::Model) -> Result<Option<Self>, RepoError> {
        let kind: ChangeKind = m
            .change_kind
            .parse()
            .map_err(|_| RepoError::CorruptRow(format!("price {} change_kind", m.id)))?;
        let target = || {
            m.target_price_id
                .ok_or_else(|| RepoError::CorruptRow(format!("change {} names no price", m.id)))
        };
        match kind {
            ChangeKind::Set => Ok(None),
            ChangeKind::Cancel => Ok(Some(Self::Cancel {
                id: m.id,
                target: target()?,
            })),
            ChangeKind::End => Ok(Some(Self::End {
                id: m.id,
                target: target()?,
                end: m
                    .effective_to
                    .ok_or_else(|| RepoError::CorruptRow(format!("end {} has no new end", m.id)))?,
            })),
        }
    }
}
/// Where a change's guards run (D-520): at its door or at submit, or at the unit's apply, where a
/// cancel's price that started since is the race's own code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Judge,
    Apply,
}

fn invalid(code: &'static str, detail: impl Into<String>) -> ApprovalError {
    ApprovalError::InvalidSubmit {
        code,
        field: price::field_of(code).into(),
        detail: detail.into(),
    }
}
fn rule(error: RuleError, id: Uuid) -> ApprovalError {
    invalid(error.code, format!("price {id}"))
}
/// Keep contention typed for the transaction's retry; a unique arbiter refuses the apply.
fn storage(error: RepoError) -> ApprovalError {
    match error {
        RepoError::Driver { source, .. } => ApprovalError::Db(source),
        RepoError::Conflict { code } => ApprovalError::ApplyRefused {
            code,
            detail: code.into(),
        },
        other => ApprovalError::Store(other.to_string()),
    }
}
/// A submit-time refusal met again at apply is an environment change.
fn applied(error: ApprovalError) -> ApprovalError {
    match error {
        ApprovalError::InvalidSubmit { code, detail, .. } => {
            ApprovalError::ApplyRefused { code, detail }
        }
        other => other,
    }
}
fn date(d: Date) -> String {
    d.to_string()
}
/// Proposed business content only: never state, lock, version or recomputed columns. The model
/// is the entry's, not the price's content (D-427).
fn after(r: &Price, note: Option<&str>) -> Value {
    json!({
        "price_book_entry_id": r.price_book_entry_id,
        "version_no": r.version_no,
        "dim_value": r.dim_value,
        "price": r.price,
        "min_fee": r.min_fee.map(|v| v.to_string()),
        "eligibility": r.eligibility.as_str(),
        "effective_from": date(r.effective_from),
        "effective_to": r.effective_to.map(date),
        "temporary_until": r.temporary_until.map(date),
        "closed_explicitly": r.closed_explicitly,
        "paired_price_id": r.paired_price_id,
        "return_of_price_id": r.return_of_price_id,
        "note": note,
    })
}
/// A `cancel` or `end` row's proposed content (D-520, D-521): what it asks of which price.
fn change_after(m: &entity::price::Model, change: &Change) -> Value {
    json!({
        "price_book_entry_id": m.price_book_entry_id,
        "change_kind": change.kind().as_str(),
        "target_price_id": change.target(),
        "dim_value": m.dim_value,
        "effective_to": change.end().map(date),
        "note": m.note,
    })
}
fn before(r: &Price) -> Value {
    json!({
        "price_id": r.id,
        "version_no": r.version_no,
        "price": r.price,
        "min_fee": r.min_fee.map(|v| v.to_string()),
        "eligibility": r.eligibility.as_str(),
        "effective_from": date(r.effective_from),
        "effective_to": r.effective_to.map(date),
    })
}
/// The entries a unit's prices belong to, from their proposed content.
fn entries_of(items: &[ItemRef]) -> BTreeSet<Uuid> {
    items
        .iter()
        .filter_map(|i| i.after["price_book_entry_id"].as_str())
        .filter_map(|id| id.parse().ok())
        .collect()
}
/// The impact object every read shows (D-392): the queue card and list, the publish-changes
/// listing and the stored snapshot. Plans are the revisions whose items name one of the entries
/// (`plans_reading`); subscriptions wait for the Subscriptions integration.
#[must_use]
pub fn impact_of(prices: usize, entries: usize, plans: &[Value]) -> Value {
    json!({
        "prices": prices,
        "entries": entries,
        "plans": plans,
        "subscriptions": SUBSCRIPTIONS_UNAVAILABLE,
    })
}
/// Every plan revision, in any state, whose items name one of the entries, as
/// `{ plan_id, code, revision_id, rev_no, state }`, by plan code and revision number (D-408). The
/// state is the one the revision reads on `today` among its plan's revisions (D-447): a live read
/// shows a due switch before it is persisted, and a snapshot records the state of its day, which
/// then stays its history.
/// Four statements whatever the number of revisions: the items, the revisions, their plans'
/// revisions and the plans (PS-14).
/// # Errors
/// Storage failures; a revision or plan an item points at that is gone is a corrupt row.
pub async fn plans_reading(
    tx: &impl DBRunner,
    tenant: Uuid,
    entries: &BTreeSet<Uuid>,
    today: Date,
) -> Result<Vec<Value>, RepoError> {
    Ok(PlansReading::load(tx, tenant, entries, today)
        .await?
        .rows(entries))
}
/// The plan revisions that read a set of entries, loaded once for many readers (the unit list's
/// page, D-458): which revisions name each entry, and each revision's row as [`plans_reading`]
/// answers it.
pub struct PlansReading {
    by_entry: BTreeMap<Uuid, BTreeSet<Uuid>>,
    rows: BTreeMap<Uuid, PlanRow>,
}
/// One revision's row, in its sort order: plan code, revision number, plan, revision, state.
type PlanRow = (String, i32, Uuid, Uuid, String);
impl PlansReading {
    /// The revisions naming any of `entries`, in [`plans_reading`]'s four statements.
    /// # Errors
    /// Storage failures; a revision or plan an item points at that is gone is a corrupt row.
    pub async fn load(
        tx: &impl DBRunner,
        tenant: Uuid,
        entries: &BTreeSet<Uuid>,
        today: Date,
    ) -> Result<Self, RepoError> {
        let scope = AccessScope::for_tenant(tenant);
        let ids: Vec<Uuid> = entries.iter().copied().collect();
        let mut by_entry: BTreeMap<Uuid, BTreeSet<Uuid>> = BTreeMap::new();
        for item in plan_item_repo::naming_entries(tx, &scope, tenant, &ids).await? {
            if let Some(entry) = item.price_book_entry_id {
                by_entry.entry(entry).or_default().insert(item.revision_id);
            }
        }
        let revisions: Vec<Uuid> = by_entry
            .values()
            .flatten()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let found = plan_revision_repo::find_many(tx, &scope, tenant, &revisions).await?;
        let held: BTreeSet<Uuid> = found.iter().map(|r| r.id).collect();
        if let Some(lost) = revisions.iter().find(|id| !held.contains(id)) {
            return Err(RepoError::CorruptRow(format!(
                "plan item names lost revision {lost}"
            )));
        }
        let plans: Vec<Uuid> = found
            .iter()
            .map(|r| r.plan_id)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let siblings = plan_revision_repo::for_plans(tx, &scope, tenant, &plans).await?;
        let effective: BTreeMap<Uuid, &'static str> =
            super::plan_revisions::effective_revisions(&siblings, today)?
                .into_iter()
                .map(|e| (e.id, e.state.as_str()))
                .collect();
        let by_id: BTreeMap<Uuid, crate::infra::storage::entity::plan::Model> =
            plan_repo::find_many(tx, &scope, tenant, &plans)
                .await?
                .into_iter()
                .map(|p| (p.id, p))
                .collect();
        let mut rows = BTreeMap::new();
        for r in found {
            let p = by_id
                .get(&r.plan_id)
                .ok_or_else(|| RepoError::CorruptRow(format!("revision {} has no plan", r.id)))?;
            let state = effective
                .get(&r.id)
                .map_or_else(|| r.state.clone(), |s| (*s).to_owned());
            rows.insert(r.id, (p.code.clone(), r.rev_no, p.id, r.id, state));
        }
        Ok(Self { by_entry, rows })
    }
    /// The rows of the revisions naming any of `entries`, each once, by plan code and revision
    /// number.
    #[must_use]
    pub fn rows(&self, entries: &BTreeSet<Uuid>) -> Vec<Value> {
        let revisions: BTreeSet<Uuid> = entries
            .iter()
            .filter_map(|e| self.by_entry.get(e))
            .flatten()
            .copied()
            .collect();
        let mut rows: Vec<_> = revisions
            .iter()
            .filter_map(|id| self.rows.get(id))
            .cloned()
            .collect();
        rows.sort();
        rows.into_iter()
            .map(|(code, rev_no, plan_id, revision_id, state)| {
                json!({
                    "plan_id": plan_id,
                    "code": code,
                    "revision_id": revision_id,
                    "rev_no": rev_no,
                    "state": state,
                })
            })
            .collect()
    }
}
/// The entries a `prices` unit's items name.
#[must_use]
pub fn entries_of_items(items: &[ItemRef]) -> BTreeSet<Uuid> {
    entries_of(items)
}
/// The live impact of a stored `prices` unit, recomputed on every read.
/// # Errors
/// Storage failures.
pub async fn live_impact(
    tx: &impl DBRunner,
    tenant: Uuid,
    items: &[ItemRef],
) -> Result<Value, RepoError> {
    let entries = entries_of(items);
    let today = OffsetDateTime::now_utc().date();
    let plans = plans_reading(tx, tenant, &entries, today).await?;
    Ok(impact_of(items.len(), entries.len(), &plans))
}
/// [`live_impact`] from a reading loaded for many units at once (D-458).
#[must_use]
pub fn impact_from(reading: &PlansReading, items: &[ItemRef]) -> Value {
    let entries = entries_of(items);
    impact_of(items.len(), entries.len(), &reading.rows(&entries))
}
fn by_entry(models: Vec<entity::price::Model>) -> BTreeMap<Uuid, Vec<entity::price::Model>> {
    let mut groups: BTreeMap<Uuid, Vec<entity::price::Model>> = BTreeMap::new();
    for m in models {
        groups.entry(m.price_book_entry_id).or_default().push(m);
    }
    groups
}

impl PricesSubject {
    /// A subject for the caller's tenant; no shift, no pulled-in partner, withdraw semantics.
    #[must_use]
    pub fn new(
        ctx: SecurityContext,
        hub: Arc<toolkit::ClientHub>,
        book_id: Uuid,
        now: OffsetDateTime,
    ) -> Self {
        let tenant_id = ctx.subject_tenant_id();
        Self {
            meter_observations: crate::infra::meter_semantics::Observations::default(),
            ctx,
            hub,
            tenant_id,
            book_id,
            now,
            common_effective_date: None,
            added_partner: Vec::new(),
            release: Release::Draft,
            refused: Arc::default(),
            review: Arc::default(),
            moved: Arc::default(),
        }
    }
    /// The approved prices outside the unit whose window or state the last `apply` changed, in
    /// ascending id: re-closed or re-opened predecessors, cancelled and ended prices (D-520,
    /// D-521). Empty before an apply.
    #[must_use]
    pub fn moved(&self) -> BTreeSet<Uuid> {
        self.moved
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    /// The Products refusal that ended the last judgement, if any; the door answers it as is.
    #[must_use]
    pub fn take_refusal(&self) -> Option<CanonicalError> {
        self.refused
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
    /// Only unavailability (5xx, timeouts, rate limits, lost races) is `REGISTRY_UNAVAILABLE`;
    /// a definite refusal (403, 404, …) passes through with its code.
    fn registry_failure(&self, error: CanonicalError) -> ApprovalError {
        if !reference_work::definite_refusal(&error) {
            return invalid("REGISTRY_UNAVAILABLE", "Products reference registry");
        }
        *self.refused.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
        invalid("REGISTRY_REFUSED", "Products refused the dated SKU read")
    }
    fn scope(&self) -> AccessScope {
        AccessScope::for_tenant(self.tenant_id)
    }
    /// The plans reading the unit's entries and each entry SKU's current descriptors, read fresh
    /// for the reviewer (D-408) and kept for `snapshot`, never in `after`. The descriptors are
    /// information, so their read is best-effort (D-416): a registry that cannot answer or refuses
    /// the caller records them `"unavailable"` and never refuses the submit, vote or reject. The
    /// reads a rule needs (a usage chain's dated metering, D-402) stay hard in `judge`.
    async fn review(&self, tx: &DbTx<'_>, entries: &BTreeSet<Uuid>) -> Result<(), ApprovalError> {
        let plans = plans_reading(tx, self.tenant_id, entries, self.now.date())
            .await
            .map_err(storage)?;
        let mut skus = BTreeSet::new();
        for id in entries {
            if let Some(entry) = price_book_entry_repo::find(tx, &self.scope(), self.tenant_id, *id)
                .await
                .map_err(storage)?
            {
                skus.insert(entry.sku_id);
            }
        }
        let descriptors =
            plan_revisions::descriptors_or_unavailable(self.meter_observations.skus(skus));
        {
            let mut review = self.review.lock().unwrap_or_else(PoisonError::into_inner);
            review.plans = plans;
            review.descriptors = descriptors;
        }
        Ok(())
    }
    /// The prices, by id, in ONE statement whatever their number (PS-39); the first id, in
    /// ascending order, that the tenant does not hold is `PRICE_NOT_FOUND`.
    async fn load_all(
        &self,
        tx: &DbTx<'_>,
        ids: impl IntoIterator<Item = Uuid>,
    ) -> Result<Vec<entity::price::Model>, ApprovalError> {
        let mut ids: Vec<Uuid> = ids.into_iter().collect();
        ids.sort_unstable();
        ids.dedup();
        let mut models = price_repo::find_many(tx, &self.scope(), self.tenant_id, &ids)
            .await
            .map_err(storage)?;
        let held: BTreeSet<Uuid> = models.iter().map(|m| m.id).collect();
        if let Some(id) = ids.iter().find(|id| !held.contains(id)) {
            return Err(invalid("PRICE_NOT_FOUND", format!("price {id}")));
        }
        models.sort_by_key(|m| m.id);
        Ok(models)
    }
    /// The SKU's metering in force on a date (D-402). A date before the SKU's first version
    /// reads that first version's metering, never "none".
    fn metering(&self, sku: Uuid, on: Date) -> Result<SkuMetering, ApprovalError> {
        self.meter_observations
            .metering(sku, on)
            .map_err(|e| self.registry_failure(e))
    }
    /// The pair guard over every link of a usage chain that touches the unit.
    fn guard(
        &self,
        sku: Uuid,
        chain: &[Price],
        unit: &BTreeSet<Uuid>,
    ) -> Result<(), ApprovalError> {
        for successor in chain {
            let Some(predecessor) = price::in_force_before(chain, successor) else {
                continue;
            };
            if !unit.contains(&successor.id) && !unit.contains(&predecessor.id) {
                continue;
            }
            let was = self.metering(sku, predecessor.effective_from)?;
            let is = self.metering(sku, successor.effective_from)?;
            price::chain_guard(ChargeKind::Usage, predecessor, &was, successor, &is)
                .map_err(|e| rule(e, successor.id))?;
        }
        Ok(())
    }
    /// Shift, validate against the CURRENT approved prices, normalise and guard one entry's prices.
    async fn judge(
        &self,
        tx: &DbTx<'_>,
        price_book_entry_id: Uuid,
        prices: &[entity::price::Model],
        shift: Option<Date>,
        stage: Stage,
    ) -> Result<Judged, ApprovalError> {
        let entry =
            price_book_entry_repo::find(tx, &self.scope(), self.tenant_id, price_book_entry_id)
                .await
                .map_err(storage)?
                .ok_or_else(|| {
                    invalid("ENTRY_NOT_FOUND", format!("entry {price_book_entry_id}"))
                })?;
        if entry.book_id != self.book_id {
            return Err(invalid(
                "PRICE_NOT_IN_BOOK",
                format!("entry {price_book_entry_id}"),
            ));
        }
        if entry.reference_state == crate::domain::price_book_entry::ReferenceState::Lost.as_str() {
            return Err(invalid(
                "ENTRY_REFERENCE_LOST",
                format!("entry {price_book_entry_id}"),
            ));
        }
        // D-522: a released entry's prices are not submitted or applied: `BOOK_ARCHIVED` while
        // its book is archived, else `ENTRY_REFERENCE_RELEASED` (an unarchive left it released).
        if entry.reference_state
            == crate::domain::price_book_entry::ReferenceState::Released.as_str()
        {
            let archived = book_repo::find(tx, &self.scope(), self.tenant_id, self.book_id)
                .await
                .map_err(storage)?
                .is_some_and(|book| book.archived_at.is_some());
            return Err(invalid(
                if archived {
                    "BOOK_ARCHIVED"
                } else {
                    "ENTRY_REFERENCE_RELEASED"
                },
                format!("entry {price_book_entry_id}"),
            ));
        }
        self.meter_observations.check(&entry).map_err(|error| {
            *self.refused.lock().unwrap_or_else(PoisonError::into_inner) = Some(error);
            invalid("METER_POLICY_REFUSED", format!("entry {}", entry.id))
        })?;
        let pc = PriceBookEntryContext::load(tx, self.tenant_id, &entry)
            .await
            .map_err(storage)?;
        let unit: BTreeSet<Uuid> = prices.iter().map(|m| m.id).collect();
        let sets: Vec<&entity::price::Model> =
            prices.iter().filter(|m| price_repo::is_price(m)).collect();
        let changes: Vec<Change> = prices
            .iter()
            .filter_map(|m| Change::of(m).transpose())
            .collect::<Result<_, _>>()
            .map_err(storage)?;
        let siblings: Vec<Price> = pc
            .domain_prices()
            .map_err(storage)?
            .into_iter()
            .filter(|r| r.state == PriceState::Approved && !unit.contains(&r.id))
            .collect();
        let drafts = sets
            .iter()
            .map(|m| price_repo::to_domain(m, pc.model))
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)?;
        let proposed =
            price::shift_selection(&drafts, shift).map_err(|e| rule(e, price_book_entry_id))?;
        let today = self.now.date();
        let mut starts = BTreeSet::new();
        for r in &proposed {
            if let Some(error) = pc.first_refusal(r, &siblings, today) {
                return Err(rule(error, r.id));
            }
            if !starts.insert((r.dim_value.clone(), r.effective_from)) {
                return Err(invalid("WINDOW_OVERLAP", format!("price {}", r.id)));
            }
        }
        // D-391: a return restores what the approved chain applies on the shifted end NOW; the
        // copy made at drafting is stale once another unit (or the common date) changed that.
        // The author re-drafts the pair.
        if let Some(stale) = proposed
            .iter()
            .find(|r| !price::temporary_is_current(&siblings, r, &proposed))
        {
            return Err(invalid("PAIR_RETURN_STALE", format!("price {}", stale.id)));
        }
        // D-406: a temporary window is not crossed, by the approved chain or by the unit itself.
        let mut around = siblings.clone();
        around.extend(proposed.iter().cloned());
        if let Some((r, promo)) = proposed
            .iter()
            .find_map(|r| price::temporary_holding(r, &around).map(|t| (r, t)))
        {
            return Err(invalid(
                "PRICE_INSIDE_TEMPORARY",
                format!("price {} starts inside temporary price {}", r.id, promo.id),
            ));
        }
        if let Some((r, start)) = proposed
            .iter()
            .find_map(|r| price::start_spanned(r, &around).map(|s| (r, s)))
        {
            return Err(invalid(
                "TEMPORARY_SPANS_A_CHANGE",
                format!(
                    "temporary price {} spans the start of price {}",
                    r.id, start.id
                ),
            ));
        }
        let mut chain = siblings;
        chain.extend(proposed.iter().cloned().map(|mut r| {
            r.state = PriceState::Approved;
            r
        }));
        // D-520, D-521: each change is judged against the chain as the unit's prices leave it, and
        // one unit names a price once, as one pending change does across units.
        let before = chain.clone();
        let mut named = BTreeSet::new();
        let mut touched: BTreeSet<Option<String>> =
            proposed.iter().map(|r| r.dim_value.clone()).collect();
        for change in &changes {
            let target = change.target();
            if !named.insert(target) {
                return Err(invalid("PRICE_CHANGE_PENDING", format!("price {target}")));
            }
            let bound = matches!(change, Change::Cancel { .. })
                && acceptance_repo::binds_price(tx, &self.scope(), self.tenant_id, target)
                    .await
                    .map_err(storage)?;
            let ended = guard_change(change, &before, &pc.prices, today, stage, bound)?;
            if let Some(named) = before.iter().find(|row| row.id == target) {
                touched.insert(named.dim_value.clone());
            }
            match ended {
                Some(closed) => {
                    if let Some(slot) = chain.iter_mut().find(|row| row.id == closed.id) {
                        *slot = closed;
                    }
                }
                None => chain.retain(|row| row.id != target),
            }
        }
        price::normalize_windows(&mut chain);
        if pc.kind == ChargeKind::Usage {
            self.guard(entry.sku_id, &chain, &unit)?;
        }
        Ok(Judged {
            stored: pc.prices,
            proposed,
            chain,
            changes,
            touched,
        })
    }
}

/// The guards of a `cancel` or an `end` (D-520, D-521): at its door, at submit and again at
/// apply. `chain` is the entry's approved prices as the unit's own prices leave them; `stored` is
/// every row of the entry; `bound` says whether a consumer's binding names the price a cancel
/// names ([`acceptance_repo::binds_price`]). An `end` answers its price explicitly closed at the
/// new end.
///
/// A cancel names an approved price that has not started, that no other pending change names
/// and that no binding names. Its `keep_for_bound` mark is not the test: the apply sets it on the
/// price before every `new` price, whether or not anyone holds it (D-520 amended). An end names an
/// approved price that has not ended by today and that no other pending change names; its new
/// end is after today, after the price's start and no later than its current end.
/// # Errors
/// A cancel: `PRICE_NOT_SCHEDULED` (not approved, or started; `PRICE_ALREADY_STARTED` at
/// [`Stage::Apply`]), `PRICE_CHANGE_PENDING`, `PRICE_BOUND` (a binding names it). An end:
/// `PRICE_ALREADY_ENDED` (not approved, or ended), `PRICE_CHANGE_PENDING`, `END_DATE_INVALID`.
pub fn guard_change(
    change: &Change,
    chain: &[Price],
    stored: &[entity::price::Model],
    today: Date,
    stage: Stage,
    bound: bool,
) -> Result<Option<Price>, ApprovalError> {
    let named = change.target();
    let refuse = |code: &'static str| invalid(code, format!("price {named}"));
    let target = chain
        .iter()
        .find(|row| row.id == named && row.state == PriceState::Approved);
    let pending = stored.iter().any(|row| {
        row.id != change.id()
            && row.target_price_id == Some(named)
            && row.state == PriceState::Pending.as_str()
    });
    match *change {
        Change::Cancel { .. } => {
            let target = target.ok_or_else(|| refuse("PRICE_NOT_SCHEDULED"))?;
            if target.effective_from <= today {
                return Err(refuse(match stage {
                    Stage::Judge => "PRICE_NOT_SCHEDULED",
                    Stage::Apply => "PRICE_ALREADY_STARTED",
                }));
            }
            if pending {
                return Err(refuse("PRICE_CHANGE_PENDING"));
            }
            if bound {
                return Err(refuse("PRICE_BOUND"));
            }
            Ok(None)
        }
        Change::End { end, .. } => {
            if target.is_none_or(|t| t.effective_to.is_some_and(|end| end <= today)) {
                return Err(refuse("PRICE_ALREADY_ENDED"));
            }
            if pending {
                return Err(refuse("PRICE_CHANGE_PENDING"));
            }
            if end <= today {
                return Err(refuse("END_DATE_INVALID"));
            }
            price::end_price(chain, named, end)
                .map(Some)
                .map_err(|error| rule(error, named))
        }
    }
}

#[async_trait::async_trait]
impl<'a> ApprovalSubject<DbTx<'a>> for PricesSubject {
    fn kind(&self) -> &'static str {
        KIND_PRICES
    }
    fn ref_type(&self) -> &'static str {
        REF_TYPE
    }
    /// The prices, their pair partners, and each price's chain predecessor on its new start.
    async fn collect(&self, tx: &DbTx<'a>, ids: &[Uuid]) -> Result<Vec<ItemRef>, ApprovalError> {
        // The asked prices once, then only the partners they name that were not asked (PS-39).
        let mut models = self.load_all(tx, ids.iter().copied()).await?;
        let asked: BTreeSet<Uuid> = models.iter().map(|m| m.id).collect();
        let partners: BTreeSet<Uuid> = models
            .iter()
            .filter_map(|m| m.paired_price_id)
            .filter(|partner| !asked.contains(partner))
            .collect();
        models.extend(self.load_all(tx, partners).await?);
        models.sort_by_key(|m| m.id);
        let mut items = Vec::new();
        let grouped = by_entry(models);
        self.review(tx, &grouped.keys().copied().collect()).await?;
        for (price_book_entry_id, prices) in grouped {
            let unit: BTreeSet<Uuid> = prices.iter().map(|m| m.id).collect();
            let entry =
                price_book_entry_repo::find(tx, &self.scope(), self.tenant_id, price_book_entry_id)
                    .await
                    .map_err(storage)?
                    .ok_or_else(|| {
                        invalid("ENTRY_NOT_FOUND", format!("entry {price_book_entry_id}"))
                    })?;
            let model = price_book_entry_repo::model_of(&entry).map_err(storage)?;
            let approved: Vec<Price> =
                price_repo::for_entry(tx, &self.scope(), self.tenant_id, price_book_entry_id)
                    .await
                    .map_err(storage)?
                    .iter()
                    .filter(|m| {
                        m.state == PriceState::Approved.as_str()
                            && price_repo::is_price(m)
                            && !unit.contains(&m.id)
                    })
                    .map(|m| price_repo::to_domain(m, model))
                    .collect::<Result<_, _>>()
                    .map_err(storage)?;
            let sets: Vec<&entity::price::Model> =
                prices.iter().filter(|m| price_repo::is_price(m)).collect();
            let drafts = sets
                .iter()
                .map(|m| price_repo::to_domain(m, model))
                .collect::<Result<Vec<_>, _>>()
                .map_err(storage)?;
            let proposed = price::shift_selection(&drafts, self.common_effective_date)
                .map_err(|e| rule(e, price_book_entry_id))?;
            let mut chain = approved.clone();
            chain.extend(proposed.iter().cloned().map(|mut r| {
                r.state = PriceState::Approved;
                r
            }));
            // A cancelled price leaves the chain, so each price's predecessor is read without it.
            let mut changes = Vec::new();
            for m in &prices {
                if let Some(change) = Change::of(m).map_err(storage)? {
                    changes.push((m, change));
                }
            }
            for (_, change) in changes
                .iter()
                .filter(|(_, c)| matches!(c, Change::Cancel { .. }))
            {
                chain.retain(|row| row.id != change.target());
            }
            price::normalize_windows(&mut chain);
            for (m, r) in sets.iter().zip(&proposed) {
                let predecessor = chain
                    .iter()
                    .find(|c| c.id == r.id)
                    .and_then(|c| price::in_force_before(&chain, c));
                items.push(ItemRef {
                    item_type: ITEM_TYPE.into(),
                    item_id: m.id,
                    created_by: m.created_by,
                    before: predecessor.map(before),
                    after: after(r, m.note.as_deref()),
                });
            }
            // D-520, D-521: a change shows the price it names as `before`, and what it asks.
            for (m, change) in changes {
                items.push(ItemRef {
                    item_type: ITEM_TYPE.into(),
                    item_id: m.id,
                    created_by: m.created_by,
                    before: approved
                        .iter()
                        .find(|r| r.id == change.target())
                        .map(before),
                    after: change_after(m, &change),
                });
            }
        }
        Ok(items)
    }
    /// Drafts only, whole pairs only, one book, and the pure rules against the current chains.
    async fn validate_submit(&self, tx: &DbTx<'a>, items: &[ItemRef]) -> Result<(), ApprovalError> {
        let ids: BTreeSet<Uuid> = items.iter().map(|i| i.item_id).collect();
        let models = self.load_all(tx, ids.iter().copied()).await?;
        for m in &models {
            if m.state != PriceState::Draft.as_str() || m.pending_unit_id.is_some() {
                return Err(invalid("PRICE_NOT_DRAFT", format!("price {}", m.id)));
            }
            if m.paired_price_id.is_some_and(|p| !ids.contains(&p)) {
                return Err(invalid("PAIR_SPLIT", format!("price {}", m.id)));
            }
        }
        for (price_book_entry_id, prices) in by_entry(models) {
            self.judge(
                tx,
                price_book_entry_id,
                &prices,
                self.common_effective_date,
                Stage::Judge,
            )
            .await?;
        }
        Ok(())
    }
    /// Conditional ownership in ascending price id; a lost write refuses the whole submit.
    async fn lock(
        &self,
        tx: &DbTx<'a>,
        unit_id: Uuid,
        items: &[ItemRef],
    ) -> Result<(), ApprovalError> {
        for m in self.load_all(tx, items.iter().map(|i| i.item_id)).await? {
            if !price_repo::try_lock(tx, &self.scope(), self.tenant_id, m.id, unit_id, m.version)
                .await
                .map_err(storage)?
            {
                return Err(ApprovalError::Locked {
                    item_type: ITEM_TYPE.into(),
                    item_id: m.id,
                });
            }
        }
        Ok(())
    }
    fn snapshot(&self, items: &[ItemRef], common_effective_date: Option<Date>) -> Value {
        let (plans, descriptors) = {
            let r = self.review.lock().unwrap_or_else(PoisonError::into_inner);
            (r.plans.clone(), r.descriptors.clone())
        };
        json!({
            "meter_evidence": self.meter_observations.audit(),
            "book_id": self.book_id,
            "common_effective_date": common_effective_date.map(date),
            "prices": items
                .iter()
                .map(|i| json!({"price_id": i.item_id, "before": i.before, "after": i.after}))
                .collect::<Vec<_>>(),
            "added_partner": self.added_partner,
            "impact": impact_of(items.len(), entries_of(items).len(), &plans),
            "descriptors": descriptors,
            "computed_at": self
                .now
                .format(&time::format_description::well_known::Rfc3339)
                .ok(),
        })
    }
    /// Re-judge every touched chain, then approve, re-close predecessors, cancel and end the
    /// prices the unit's changes name (D-520, D-521) and mark `keep_for_bound`; any refusal rolls
    /// the whole unit back as `APPLY_REFUSED`. The prices outside the unit whose window or state
    /// moved are kept for the event ([`Self::moved`]).
    async fn apply(
        &self,
        tx: &DbTx<'a>,
        unit: &Unit,
        items: &[ItemRef],
    ) -> Result<(), ApprovalError> {
        self.moved
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        let models = self.load_all(tx, items.iter().map(|i| i.item_id)).await?;
        if let Some(m) = models
            .iter()
            .find(|m| m.pending_unit_id != Some(unit.id) || m.state != PriceState::Pending.as_str())
        {
            return Err(ApprovalError::ApplyRefused {
                code: "PRICE_NOT_PENDING",
                detail: format!("price {}", m.id),
            });
        }
        let mut moved = BTreeSet::new();
        // Entries in ascending id: two batches over the same entries meet in one order.
        for (price_book_entry_id, prices) in by_entry(models) {
            let judged = self
                .judge(
                    tx,
                    price_book_entry_id,
                    &prices,
                    unit.common_effective_date,
                    Stage::Apply,
                )
                .await
                .map_err(applied)?;
            let normalised = |id: Uuid| judged.chain.iter().find(|c| c.id == id);
            // The current predecessor of EVERY `new` price of a touched chain binds renewals,
            // whether the `new` price is in this unit or was approved earlier and a price of this
            // unit now sits in front of it, or a price this unit cancels no longer does. A mark
            // is never cleared.
            let mut keep = BTreeSet::new();
            for c in judged.chain.iter().filter(|c| {
                c.eligibility == Eligibility::New && judged.touched.contains(&c.dim_value)
            }) {
                if let Some(predecessor) = price::in_force_before(&judged.chain, c) {
                    keep.insert(predecessor.id);
                }
            }
            for r in &judged.proposed {
                price_repo::approve(
                    tx,
                    &self.scope(),
                    self.tenant_id,
                    r.id,
                    unit.id,
                    price_repo::Approval {
                        effective_from: r.effective_from,
                        effective_to: normalised(r.id).and_then(|c| c.effective_to),
                        temporary_until: r.temporary_until,
                        keep_for_bound: keep.contains(&r.id),
                    },
                    self.now,
                )
                .await
                .map_err(storage)?;
            }
            let changed = |kind: ChangeKind| -> BTreeSet<Uuid> {
                judged
                    .changes
                    .iter()
                    .filter(|c| c.kind() == kind)
                    .map(Change::target)
                    .collect()
            };
            let (cancelled, ended) = (changed(ChangeKind::Cancel), changed(ChangeKind::End));
            // A concurrent write to a price this apply re-closes, cancels or ends is contention:
            // the transaction retries and judges again.
            let contended = |e: RepoError| match e {
                RepoError::Conflict { .. } => ApprovalError::Contended,
                other => storage(other),
            };
            for stored in judged
                .stored
                .iter()
                .filter(|m| m.state == PriceState::Approved.as_str() && price_repo::is_price(m))
            {
                // D-520: the cancelled price leaves the chain, and this loop re-closes the price
                // before it onto the normalised end, as it re-closes every other one.
                if cancelled.contains(&stored.id) {
                    price_repo::cancel(
                        tx,
                        &self.scope(),
                        self.tenant_id,
                        stored.id,
                        stored.version,
                        unit.id,
                        self.now,
                    )
                    .await
                    .map_err(contended)?;
                    moved.insert(stored.id);
                    continue;
                }
                let Some(chain) = normalised(stored.id) else {
                    continue;
                };
                let keep_for_bound = stored.keep_for_bound || keep.contains(&stored.id);
                if ended.contains(&stored.id) {
                    // D-521: the explicit end, unless a successor already starts inside it.
                    let end = chain.effective_to.ok_or_else(|| {
                        ApprovalError::Store(format!("price {} lost its new end", stored.id))
                    })?;
                    price_repo::close_explicitly(
                        tx,
                        &self.scope(),
                        self.tenant_id,
                        stored.id,
                        stored.version,
                        end,
                        keep_for_bound,
                        self.now,
                    )
                    .await
                    .map_err(contended)?;
                    moved.insert(stored.id);
                } else if chain.effective_to != stored.effective_to
                    || keep_for_bound != stored.keep_for_bound
                {
                    // Only a moved end is news for the event; a new `keep_for_bound` mark is not.
                    if chain.effective_to != stored.effective_to {
                        moved.insert(stored.id);
                    }
                    price_repo::set_window(
                        tx,
                        &self.scope(),
                        self.tenant_id,
                        stored.id,
                        stored.version,
                        chain.effective_to,
                        keep_for_bound,
                        self.now,
                    )
                    .await
                    .map_err(contended)?;
                }
            }
            // A change row is approved as it was written: it is the record of what the unit did,
            // and the approved-start index does not count it (D-520, D-521).
            for m in prices.iter().filter(|m| !price_repo::is_price(m)) {
                price_repo::approve(
                    tx,
                    &self.scope(),
                    self.tenant_id,
                    m.id,
                    unit.id,
                    price_repo::Approval {
                        effective_from: m.effective_from,
                        effective_to: m.effective_to,
                        temporary_until: None,
                        keep_for_bound: false,
                    },
                    self.now,
                )
                .await
                .map_err(storage)?;
            }
        }
        *self.moved.lock().unwrap_or_else(PoisonError::into_inner) = moved;
        Ok(())
    }
    async fn unlock(
        &self,
        tx: &DbTx<'a>,
        unit: &Unit,
        items: &[ItemRef],
        approved: bool,
    ) -> Result<(), ApprovalError> {
        let outcome = match (approved, self.release) {
            (true, _) => price_repo::Unlock::Approved,
            (false, Release::Draft) => price_repo::Unlock::Draft,
            (false, Release::Rejected) => price_repo::Unlock::Rejected,
        };
        for item in items {
            price_repo::unlock(
                tx,
                &self.scope(),
                self.tenant_id,
                item.item_id,
                unit.id,
                outcome,
            )
            .await
            .map_err(|e| match e {
                RepoError::Conflict { .. } => {
                    ApprovalError::Store("price lock is not owned by this unit".into())
                }
                other => storage(other),
            })?;
        }
        Ok(())
    }
}
