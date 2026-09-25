//! `LiveTenantFilter` — the platform-registry anchor for the gear's
//! all-tenants background sweeps.
//!
//! **Why this exists.** The ledger's own tables are the wrong source of truth
//! for "which tenants exist". Ledger data is append-only and is never cleaned,
//! so once a tenant has posted a single `journal_entry` it stays in every
//! ledger-derived tenant enumeration **forever** — after it is soft-deleted,
//! and after it is hard-deleted from `public.tenants` outright. A sweep that
//! enumerates from ledger data and then appends one row per tenant per tick
//! feeds itself: it enumerates from a table that never shrinks and writes into
//! another table that never shrinks. That is precisely how
//! `bss.ledger_reconciliation_run` reached 29 GB / 61M rows on stage1, with
//! ~99.9% of the rows belonging to tenants that no longer exist (68%
//! soft-deleted, 28% absent from the registry entirely).
//!
//! This port re-anchors those sweeps on the **platform tenant registry** (the
//! `tenant-resolver` gear, backed in-process by AM's `tenants` /
//! `tenant_closure`): a candidate set derived from ledger data is intersected
//! with the tenants the registry still reports as live, and only the
//! intersection is swept.
//!
//! **Live** means `Active` **or** `Suspended`. A suspended tenant still owns
//! real money — its open period must still tie out, and it can be unsuspended
//! — so suspension is not a reason to stop reconciling. `Deleted` is terminal
//! (AM rejects every lifecycle transition out of it during the retention
//! window), so a soft-deleted tenant will never post again and needs no further
//! reconciliation; an id the registry does not know at all is likewise dead.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use tenant_resolver_sdk::{GetTenantsOptions, TenantId, TenantResolverClient, TenantStatus};
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// Statuses whose tenants still own a live ledger and must keep being swept.
/// `Deleted` (and "absent from the registry") is the complement — see the
/// module docs for why suspension is deliberately NOT a reason to stop.
const LIVE_STATUSES: [TenantStatus; 2] = [TenantStatus::Active, TenantStatus::Suspended];

/// Tenant ids per registry lookup. The registry resolves a batch with one
/// `WHERE id IN (…)` — one bind parameter per id — so the batch must stay well
/// under Postgres' 65,535-parameter ceiling. A fleet with more tenants than
/// this is split across several round trips rather than failing at the driver.
const LOOKUP_CHUNK: usize = 500;

/// Narrow port: intersect a ledger-derived candidate set with the tenants the
/// platform registry still reports as live.
///
/// Adapting the registry behind a one-method port keeps the background sweeps
/// unit-testable without faking the whole `TenantResolverClient` surface (the
/// same shape [`crate::infra::seller_guard::TenantTypeReader`] uses for AM's
/// tenant-type read).
#[async_trait]
pub trait LiveTenantFilter: Send + Sync {
    /// The subset of `candidates` that the registry reports as live (present
    /// and not soft-deleted). Ids the registry does not know are simply
    /// absent from the result.
    ///
    /// # Errors
    /// The registry read failed (no resolver plugin bound, transport /
    /// storage fault). Callers treat this as "lifecycle unknown" and MUST NOT
    /// read an empty set as "every tenant is dead" — see
    /// [`crate::infra::reconciliation::ReconciliationFramework::run`] for the
    /// fail-safe.
    async fn live_tenants(&self, candidates: &[Uuid]) -> anyhow::Result<HashSet<Uuid>>;
}

/// [`LiveTenantFilter`] backed by the platform [`TenantResolverClient`].
pub struct ResolverLiveTenantFilter {
    resolver: Arc<dyn TenantResolverClient>,
}

impl ResolverLiveTenantFilter {
    /// Build the filter over the resolved tenant-resolver client.
    #[must_use]
    pub fn new(resolver: Arc<dyn TenantResolverClient>) -> Self {
        Self { resolver }
    }
}

#[async_trait]
impl LiveTenantFilter for ResolverLiveTenantFilter {
    async fn live_tenants(&self, candidates: &[Uuid]) -> anyhow::Result<HashSet<Uuid>> {
        if candidates.is_empty() {
            return Ok(HashSet::new());
        }
        // Background sweep: no caller identity to carry. The resolver contract
        // delegates authorization to its plugin, and the in-process (AM-backed)
        // plugin reads the registry unconditionally — the trust boundary is the
        // gateway, not this call (AM `tr_plugin::PluginImpl` docs).
        let ctx = SecurityContext::anonymous();
        let options = GetTenantsOptions {
            status: LIVE_STATUSES.to_vec(),
        };
        let mut live = HashSet::with_capacity(candidates.len());
        for chunk in candidates.chunks(LOOKUP_CHUNK) {
            let ids: Vec<TenantId> = chunk.iter().copied().map(TenantId).collect();
            let found = self
                .resolver
                .get_tenants(&ctx, &ids, &options)
                .await
                .map_err(|e| {
                    anyhow::anyhow!("tenant registry lookup ({} candidates): {e}", ids.len())
                })?;
            live.extend(found.into_iter().map(|t| t.id.0));
        }
        Ok(live)
    }
}

#[cfg(test)]
#[path = "tenant_lifecycle_tests.rs"]
mod tests;
