//! Bounded recovery and periodic receipt reconciliation under the pricing system actor, and the
//! switch of scheduled plan revisions on their date.
//!
//! Reconciliation scans both kinds of reference (D-407), each with its own cursor: confirmed and
//! lost price book entries, and confirmed and lost plan items of every revision state (D-414).
//! The switch duty (D-450) runs first in its tick, with its own error handling, so neither the
//! reference duties' scans nor a failing reconciliation can skip it. The tick count moves right
//! after it, so a scan that keeps failing cannot freeze the count off the switch's period.
//!
//! @cpt-dod:cpt-cf-bss-pricing-dod-confirmation-retry:p1
use super::{
    plan_revisions,
    reference_work::{self, Caller, Clock},
    storage::{
        entity::{plan_item, price_book_entry},
        repo::{
            plan_item_repo, plan_revision_repo, price_book_entry_repo, reference_op_repo as ops,
        },
    },
};
use crate::{
    api::rest::authoring::{
        AuthoringState,
        support::{self, DoorError},
    },
    domain::{
        plan::ReferenceState as ItemReferenceState,
        price_book_entry::{ReferenceState, charge_kind_for},
        reference_op::{OpKind, RefKind},
    },
};
use bss_products_sdk::{
    PRICING_SYSTEM_ACTOR,
    models::{Lifecycle, ReferenceState as RegistryState, SkuType},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_db::secure::AccessScope;
use toolkit_security::SecurityContext;
use uuid::Uuid;
/// A fixed identity scoped anew for every operation's tenant.
/// # Errors
/// Propagates invalid security context construction.
pub fn system_actor(tenant: Uuid) -> Result<SecurityContext, CanonicalError> {
    SecurityContext::builder()
        .subject_id(PRICING_SYSTEM_ACTOR)
        .subject_tenant_id(tenant)
        .subject_type("bss-pricing.system")
        .build()
        .map_err(|_| CanonicalError::internal("pricing recovery identity failed").create())
}
/// The switch duty's period in ticks (D-450): once a minute at the gear's one-second tick.
pub const SWITCH_EVERY: u64 = 60;
/// One gear-owned ticker. A cursor per reference kind bounds reconciliation across ticks.
pub struct Ticker {
    state: Arc<AuthoringState>,
    clock: Arc<dyn Clock>,
    limit: u64,
    reconcile_every: u64,
    switch_every: u64,
    ticks: u64,
    cursors: Cursors,
}
/// Where each kind's reconciliation scan resumes: after this id, or from the start.
#[derive(Debug, Clone, Copy, Default)]
struct Cursors {
    entry: Option<Uuid>,
    item: Option<Uuid>,
}
/// A reference reconciliation holds, of either kind.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Held {
    Entry(price_book_entry::Model),
    Item(plan_item::Model),
}
impl Held {
    fn tenant(&self) -> Uuid {
        match self {
            Self::Entry(entry) => entry.tenant_id,
            Self::Item(item) => item.tenant_id,
        }
    }
    fn lost(&self) -> bool {
        match self {
            Self::Entry(entry) => entry.reference_state == ReferenceState::Lost.as_str(),
            Self::Item(item) => item.reference_state == ItemReferenceState::Lost.as_str(),
        }
    }
    fn receipt(&self) -> Option<Uuid> {
        match self {
            Self::Entry(entry) => Some(entry.reservation_id),
            Self::Item(item) => item.reservation_id,
        }
    }
}
/// A batch's next cursor: its last id when it was full, otherwise the start again.
fn next_cursor<T>(batch: &[T], limit: u64, id: impl Fn(&T) -> Uuid) -> Option<Uuid> {
    if u64::try_from(batch.len()).unwrap_or(u64::MAX) == limit {
        batch.last().map(id)
    } else {
        None
    }
}
impl Ticker {
    /// Construct a bounded ticker; tests inject a clock with no jitter.
    #[must_use]
    pub fn new(
        state: Arc<AuthoringState>,
        clock: Arc<dyn Clock>,
        limit: u64,
        reconcile_every: u64,
    ) -> Self {
        Self {
            state,
            clock,
            limit: limit.clamp(1, 1000),
            reconcile_every: reconcile_every.max(1),
            switch_every: SWITCH_EVERY,
            ticks: 0,
            cursors: Cursors::default(),
        }
    }
    /// Run the switch duty every `n` ticks instead of [`SWITCH_EVERY`]; tests tick it at once.
    #[must_use]
    pub fn switch_every(mut self, n: u64) -> Self {
        self.switch_every = n.max(1);
        self
    }
    /// Switch the due scheduled plan revisions on the first tick and every [`SWITCH_EVERY`] after
    /// it, then resume a bounded due batch, then reconcile a bounded confirmed batch every N ticks.
    /// # Errors
    /// The reference duties' scan failures are surfaced; per-op failures remain due at their
    /// scheduled retry. The switch surfaces nothing: it warns and runs again on its next tick.
    // cancel-safe: `serve` drops this future at shutdown at any await, a Products call inside a
    // drive or an open transaction included. A dropped transaction rolls back; every op is a
    // durable journal row written before its registry call, advanced by compare-and-swap, and
    // due again after `IN_FLIGHT_GRACE`, so a drop is a crash the next tick recovers (D-401).
    // The in-memory tick count and cursors only pace the duties.
    pub async fn tick(&mut self) -> Result<(), CanonicalError> {
        // D-450, plan rev 2 M4: first, and never through `?`.
        if self.ticks.is_multiple_of(self.switch_every) {
            self.switch().await;
        }
        // The count moves before any duty that can return early, so a scan that keeps failing
        // cannot hold it off the switch's period (phase 8 review B1).
        self.ticks = self.ticks.wrapping_add(1);
        // Only this trusted scheduler scans all tenants. Every mutation and Products call
        // below has a tenant-only scope and the fixed pricing system actor.
        let due = ops::due(
            &self
                .state
                .db
                .conn()
                .map_err(|e| CanonicalError::from(DoorError::from(e)))?,
            &AccessScope::allow_all(),
            self.clock.now(),
            self.limit,
        )
        .await
        .map_err(|e| CanonicalError::from(DoorError::from(e)))?;
        for op in due {
            let ctx = system_actor(op.tenant_id)?;
            if let Err(error) = reference_work::drive(
                &self.state,
                &ctx,
                op.op_id,
                self.clock.clone(),
                Caller::Ticker,
            )
            .await
            {
                tracing::warn!(op_id=%op.op_id, attempts=op.attempts, error=%error, diagnostic=error.diagnostic().unwrap_or_default(), "pricing reference recovery deferred");
            }
        }
        if self.ticks.is_multiple_of(self.reconcile_every) {
            self.reconcile().await?;
        }
        Ok(())
    }
    /// The switch duty (D-450): every plan with a due scheduled revision, across tenants (at most
    /// `limit` revisions, by `available_from` then id), is switched in its own transaction with its
    /// event and its audit row ([`plan_revisions::catch_up`]). A failure is a warning for its plan,
    /// and the other plans go on; the next cycle finds what is still due.
    async fn switch(&self) {
        // @cpt-begin:cpt-cf-bss-pricing-algo-plans-revision-switch:p1:inst-plans-revision-switch-1
        let now = self.clock.now();
        let due = match self.due_plans(now).await {
            Ok(due) => due,
            Err(error) => {
                tracing::warn!(error=%error, "pricing plan switch scan deferred");
                return;
            }
        };
        for (tenant, plan) in due {
            if let Err(error) = self.switch_plan(tenant, plan, now).await {
                tracing::warn!(%tenant, plan_id=%plan, error=%error, diagnostic=error.diagnostic().unwrap_or_default(), "pricing plan switch deferred");
            }
        }
        // @cpt-end:cpt-cf-bss-pricing-algo-plans-revision-switch:p1:inst-plans-revision-switch-1
    }
    /// The plans of the due scan on the UTC day of `now`, each once, in the scan's order.
    async fn due_plans(&self, now: time::OffsetDateTime) -> Result<Vec<(Uuid, Uuid)>, String> {
        let conn = self.state.db.conn().map_err(|e| e.to_string())?;
        let today = now.to_offset(time::UtcOffset::UTC).date();
        let due = plan_revision_repo::due_scheduled(&conn, today, self.limit)
            .await
            .map_err(|e| e.to_string())?;
        let mut seen = BTreeSet::new();
        Ok(due
            .into_iter()
            .map(|r| (r.tenant_id, r.plan_id))
            .filter(|key| seen.insert(*key))
            .collect())
    }
    /// One plan's switch in its own transaction.
    async fn switch_plan(
        &self,
        tenant: Uuid,
        plan: Uuid,
        now: time::OffsetDateTime,
    ) -> Result<(), CanonicalError> {
        let db = self.state.db.db();
        support::transaction_with_events(&db, &self.state.outbox, move |tx, outbox| {
            Box::pin(async move {
                plan_revisions::catch_up(tx, &outbox, tenant, plan, now, Uuid::now_v7()).await
            })
        })
        .await
        .map(|_| ())
    }
    async fn reconcile(&mut self) -> Result<(), CanonicalError> {
        let conn = self
            .state
            .db
            .conn()
            .map_err(|e| CanonicalError::from(DoorError::from(e)))?;
        let entries = price_book_entry_repo::reconcile_batch(
            &conn,
            &AccessScope::allow_all(),
            self.cursors.entry,
            self.limit,
        )
        .await
        .map_err(|e| CanonicalError::from(DoorError::from(e)))?;
        let items = plan_item_repo::reconcile_batch(
            &conn,
            &AccessScope::allow_all(),
            self.cursors.item,
            self.limit,
        )
        .await
        .map_err(|e| CanonicalError::from(DoorError::from(e)))?;
        let next = Cursors {
            entry: next_cursor(&entries, self.limit, |e| e.id),
            item: next_cursor(&items, self.limit, |i| i.id),
        };
        let mut tenants: BTreeMap<Uuid, Vec<Held>> = BTreeMap::new();
        for held in entries
            .into_iter()
            .map(Held::Entry)
            .chain(items.into_iter().map(Held::Item))
        {
            tenants.entry(held.tenant()).or_default().push(held);
        }
        if tenants.is_empty() {
            self.cursors = Cursors::default();
            return Ok(());
        }
        let registry = super::reference_registry::resolve(&self.state.hub)?;
        for (tenant, held) in tenants {
            // One tenant's divergence (an unreachable or disagreeing registry, a storage
            // error) never halts reconciliation for the others; the cursors move on.
            if let Err(error) = self.reconcile_tenant(registry.as_ref(), tenant, held).await {
                tracing::warn!(%tenant, error=%error, diagnostic=error.diagnostic().unwrap_or_default(), "pricing reconciliation skipped a tenant");
            }
        }
        self.cursors = next;
        Ok(())
    }
    /// One tenant's slice of the batch: re-reserve every confirmed reference whose receipt
    /// Products reports released and every lost reference whose SKU admits a reservation again.
    async fn reconcile_tenant(
        &self,
        registry: &dyn bss_products_sdk::ReferenceRegistryV1,
        tenant: Uuid,
        held: Vec<Held>,
    ) -> Result<(), CanonicalError> {
        let ctx = system_actor(tenant)?;
        let (lost, confirmed): (Vec<_>, Vec<_>) = held.into_iter().partition(Held::lost);
        let mut due = Vec::new();
        if !confirmed.is_empty() {
            let ids: Vec<_> = confirmed.iter().filter_map(Held::receipt).collect();
            let states = receipt_states(registry, &ctx, tenant, &ids).await?;
            due.extend(confirmed.into_iter().filter(|held| {
                states.iter().any(|(id, state)| {
                    Some(*id) == held.receipt() && *state == RegistryState::Released
                })
            }));
        }
        for held in lost {
            if admits(registry, &ctx, &held).await {
                due.push(held);
            }
        }
        for held in due {
            self.rereserve(&ctx, held).await?;
        }
        Ok(())
    }
    /// Mint one re-reservation and drive it now; a deferred op stays durable and due.
    async fn rereserve(&self, ctx: &SecurityContext, held: Held) -> Result<(), CanonicalError> {
        let Some(id) = self.begin_rereserve(ctx, held).await? else {
            return Ok(());
        };
        if let Err(error) =
            reference_work::drive(&self.state, ctx, id, self.clock.clone(), Caller::Ticker).await
        {
            tracing::warn!(op_id=%id, error=%error, diagnostic=error.diagnostic().unwrap_or_default(), "pricing re-reservation deferred");
        }
        Ok(())
    }
    /// Start one re-reservation: the rereserve claim. A confirmed reference is claimed by moving
    /// it to `confirmation_pending` at its observed version; a lost one stays lost (an entry
    /// admits no prices, an item keeps its revision's checks red) until the new reservation is
    /// written, and one open op per reference is the guard.
    async fn begin_rereserve(
        &self,
        ctx: &SecurityContext,
        observed: Held,
    ) -> Result<Option<Uuid>, CanonicalError> {
        let (ctx, now) = (ctx.clone(), self.clock.now());
        support::transaction(&self.state.db.db(), move |tx| {
            let (ctx, observed) = (ctx.clone(), observed.clone());
            Box::pin(async move {
                let scope = AccessScope::for_tenant(ctx.subject_tenant_id());
                let due = now + reference_work::IN_FLIGHT_GRACE;
                let (kind, id, op) = match &observed {
                    Held::Entry(entry) => {
                        let current =
                            price_book_entry_repo::find(tx, &scope, entry.tenant_id, entry.id)
                                .await?;
                        if current.as_ref() != Some(entry) {
                            return Ok(None);
                        }
                        (
                            RefKind::Entry,
                            entry.id,
                            reference_work::rereserve_op(&ctx, entry, now, due)?,
                        )
                    }
                    Held::Item(item) => {
                        let current =
                            plan_item_repo::find(tx, &scope, item.tenant_id, item.id).await?;
                        if current.as_ref() != Some(item) {
                            return Ok(None);
                        }
                        (
                            RefKind::PlanItem,
                            item.id,
                            reference_work::plan_item::rereserve_op(&ctx, item, now, due)?,
                        )
                    }
                };
                let op_id = op.op_id;
                if observed.lost() {
                    if ops::open_for_ref(tx, &scope, observed.tenant(), kind, id, OpKind::Rereserve)
                        .await?
                    {
                        return Ok(None);
                    }
                } else {
                    // Claim this reference for reconciliation atomically; another ticker cannot
                    // mint competing recovery work and a removal cannot strand a new receipt.
                    match &observed {
                        Held::Entry(entry) => {
                            price_book_entry_repo::set_reference(
                                tx,
                                &scope,
                                entry.tenant_id,
                                entry.id,
                                entry.version,
                                ReferenceState::ConfirmationPending,
                                entry.reservation_id,
                                now,
                            )
                            .await?;
                        }
                        Held::Item(item) => {
                            plan_item_repo::set_reference(
                                tx,
                                &scope,
                                item.tenant_id,
                                item.id,
                                item.version,
                                ItemReferenceState::ConfirmationPending,
                                item.reservation_id,
                                now,
                            )
                            .await?;
                        }
                    }
                }
                ops::insert(tx, &scope, op).await?;
                Ok(Some(op_id))
            })
        })
        .await
    }
}
/// Products' view of confirmed receipts. A batch answered 404 names a reservation Products
/// does not know (for example after a restore): each id is then asked alone, and an unknown
/// one counts as released, so its entry is re-reserved like any other released receipt.
async fn receipt_states(
    registry: &dyn bss_products_sdk::ReferenceRegistryV1,
    ctx: &SecurityContext,
    tenant: Uuid,
    ids: &[Uuid],
) -> Result<Vec<(Uuid, RegistryState)>, CanonicalError> {
    match registry.states(ctx, tenant, ids).await {
        Err(error) if error.status_code() == 404 => {}
        other => return other,
    }
    let mut states = Vec::with_capacity(ids.len());
    for id in ids {
        match registry.states(ctx, tenant, &[*id]).await {
            Ok(found) => states.extend(found),
            Err(error) if error.status_code() == 404 => states.push((*id, RegistryState::Released)),
            Err(error) => return Err(error),
        }
    }
    Ok(states)
}
/// Whether a lost reference's SKU admits its reservation again: published or deprecated, not
/// fenced, and of a type the reference can hold. An entry needs its own charge kind (a changed
/// type cannot be healed by a reservation); an item needs any type but a bundle.
async fn admits(
    registry: &dyn bss_products_sdk::ReferenceRegistryV1,
    ctx: &SecurityContext,
    held: &Held,
) -> bool {
    let (kind, id, tenant, sku) = match held {
        Held::Entry(entry) => (RefKind::Entry, entry.id, entry.tenant_id, entry.sku_id),
        Held::Item(item) => (RefKind::PlanItem, item.id, item.tenant_id, item.sku_id),
    };
    match registry.sku_for_write(ctx, tenant, sku).await {
        Ok(sku) => {
            matches!(sku.lifecycle, Lifecycle::Published | Lifecycle::Deprecated)
                && !sku.type_change_pending
                && !sku.retire_pending
                && match held {
                    Held::Entry(entry) => charge_kind_for(sku.r#type)
                        .is_ok_and(|kind| kind.as_str() == entry.charge_kind),
                    Held::Item(_) => sku.r#type != SkuType::Bundle,
                }
        }
        Err(error) => {
            tracing::warn!(ref_kind=kind.as_str(), ref_id=%id, error=%error, diagnostic=error.diagnostic().unwrap_or_default(), "pricing lost-reference check deferred");
            false
        }
    }
}
