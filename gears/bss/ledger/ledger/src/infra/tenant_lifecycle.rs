//! Tenant lifecycle — the platform-registry anchor for the gear's all-tenants
//! background sweeps.
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
//! This module re-anchors those sweeps on the **platform tenant registry**
//! (the `tenant-resolver` gear). A candidate set derived from ledger data is
//! classified against the registry, and [`plan_lifecycle`] turns the answer
//! into what to reconcile and what may be reclaimed.
//!
//! **Positive evidence only for deletes.** The resolver contract answers
//! unknown ids by omission, and which ids a plugin knows is deployment-shaped:
//! the AM-backed plugin reads `public.tenants`, but `static-tr-plugin` knows
//! only its configured list and `single-tenant-tr-plugin` only the caller's
//! tenant. An omission is therefore NOT proof of deletion. So:
//!
//! * `Active` / `Suspended` → **live**, reconciled. A suspended tenant still
//!   owns real money — its open period must still tie out, and it can be
//!   unsuspended.
//! * `Deleted` → **deleted**, not reconciled, and its uneventful runs may be
//!   reclaimed. `Deleted` is terminal in AM (every lifecycle transition out of
//!   it is rejected), so the tenant will never post again.
//! * omitted → **unregistered**, not reconciled when
//!   `recon.unregistered_tenants` is `skip` (the default: with an
//!   authoritative registry this is the hard-deleted case), but **never**
//!   purged.
//! * the registry recognised **none** of a non-empty candidate set → the
//!   answer is treated as unavailable (the same as a failed read): a scoped or
//!   misbound plugin answers that way, and reading it literally would stop
//!   reconciling the whole fleet.

use std::collections::HashMap;
use std::fmt;
use std::hash::BuildHasher;
use std::sync::Arc;

use async_trait::async_trait;
use tenant_resolver_sdk::{GetTenantsOptions, TenantId, TenantResolverClient, TenantStatus};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::config::UnregisteredTenants;

/// Tenant ids per registry lookup. The AM-backed registry resolves a batch with
/// one `WHERE id IN (…)` — one bind parameter per id — so the batch must stay
/// well under Postgres' 65,535-parameter ceiling. A fleet with more tenants
/// than this is split across several round trips rather than failing at the
/// driver.
const LOOKUP_CHUNK: usize = 500;

/// What the platform registry says about one tenant it knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryLifecycle {
    /// `Active` or `Suspended` — still owns a live ledger; keep sweeping it.
    Live,
    /// Soft-deleted (`Deleted`) — terminal; never posts again.
    Deleted,
}

impl From<TenantStatus> for RegistryLifecycle {
    fn from(status: TenantStatus) -> Self {
        match status {
            TenantStatus::Active | TenantStatus::Suspended => Self::Live,
            TenantStatus::Deleted => Self::Deleted,
        }
    }
}

/// Narrow port: classify a ledger-derived candidate set against the platform
/// tenant registry.
///
/// Adapting the registry behind a one-method port keeps the background sweeps
/// unit-testable without faking the whole `TenantResolverClient` surface (the
/// same shape [`crate::infra::seller_guard::TenantTypeReader`] uses for AM's
/// tenant-type read).
#[async_trait]
pub trait TenantLifecycleReader: Send + Sync {
    /// The registry lifecycle of every candidate the registry knows, in any
    /// status. Ids the registry does not know are absent from the map.
    ///
    /// # Errors
    /// The registry read failed (no resolver plugin bound, transport /
    /// storage fault). Callers treat this as "lifecycle unknown" — see
    /// [`plan_lifecycle`].
    async fn lifecycles(
        &self,
        candidates: &[Uuid],
    ) -> anyhow::Result<HashMap<Uuid, RegistryLifecycle>>;
}

/// [`TenantLifecycleReader`] backed by the platform [`TenantResolverClient`].
pub struct ResolverTenantLifecycleReader {
    resolver: Arc<dyn TenantResolverClient>,
}

impl ResolverTenantLifecycleReader {
    /// Build the reader over the resolved tenant-resolver client.
    #[must_use]
    pub fn new(resolver: Arc<dyn TenantResolverClient>) -> Self {
        Self { resolver }
    }
}

#[async_trait]
impl TenantLifecycleReader for ResolverTenantLifecycleReader {
    async fn lifecycles(
        &self,
        candidates: &[Uuid],
    ) -> anyhow::Result<HashMap<Uuid, RegistryLifecycle>> {
        if candidates.is_empty() {
            return Ok(HashMap::new());
        }
        // Background sweep: no caller identity to carry. The AM-backed plugin
        // reads the registry unconditionally (the trust boundary is the
        // gateway, AM `tr_plugin::PluginImpl` docs). A plugin that scopes its
        // answer by the caller instead (single-tenant) recognises nothing for
        // this context, which `plan_lifecycle` treats as "unavailable" rather
        // than "everyone is gone".
        let ctx = SecurityContext::anonymous();
        // Every status: `Deleted` must come back as `Deleted`, so that only a
        // positive answer — never an omission — makes a tenant purgeable.
        let options = GetTenantsOptions { status: Vec::new() };
        let mut known = HashMap::with_capacity(candidates.len());
        for chunk in candidates.chunks(LOOKUP_CHUNK) {
            let ids: Vec<TenantId> = chunk.iter().copied().map(TenantId).collect();
            let found = self
                .resolver
                .get_tenants(&ctx, &ids, &options)
                .await
                .map_err(|e| {
                    anyhow::anyhow!("tenant registry lookup ({} candidates): {e}", ids.len())
                })?;
            known.extend(
                found
                    .into_iter()
                    .map(|t| (t.id.0, RegistryLifecycle::from(t.status))),
            );
        }
        Ok(known)
    }
}

/// How a sweep treats its candidate set this tick.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LifecyclePlan {
    /// Tenants to sweep: live ones, plus unregistered ones under
    /// [`UnregisteredTenants::Reconcile`].
    pub reconcile: Vec<Uuid>,
    /// Tenants the registry reports soft-deleted: skipped, and the only ones
    /// whose uneventful runs may be reclaimed.
    pub deleted: Vec<Uuid>,
    /// Tenants the registry did not return, skipped under
    /// [`UnregisteredTenants::Skip`]. Never purged — an omission is not proof.
    pub unregistered: Vec<Uuid>,
}

impl LifecyclePlan {
    /// The fail-safe plan: sweep every candidate, skip and reclaim nothing.
    #[must_use]
    pub fn reconcile_all(candidates: &[Uuid]) -> Self {
        Self {
            reconcile: candidates.to_vec(),
            ..Self::default()
        }
    }
}

/// Why the registry's answer could not be used this tick.
#[derive(Debug)]
pub enum LifecycleUnavailable {
    /// The registry read itself failed.
    ReadFailed(anyhow::Error),
    /// The read succeeded but recognised none of a non-empty candidate set —
    /// what a caller-scoped or misbound plugin returns, not a plausible fleet.
    NoneRecognised {
        /// Size of the candidate set the registry was asked about.
        candidates: usize,
    },
}

impl LifecycleUnavailable {
    /// Stable metric label for the reason.
    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self {
            Self::ReadFailed(_) => "read_failed",
            Self::NoneRecognised { .. } => "none_recognised",
        }
    }
}

impl fmt::Display for LifecycleUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadFailed(e) => write!(f, "tenant-registry read failed: {e}"),
            Self::NoneRecognised { candidates } => write!(
                f,
                "tenant registry recognised none of {candidates} candidates \
                 (caller-scoped or misbound resolver plugin?)"
            ),
        }
    }
}

/// Turn the registry's answer about `candidates` into this tick's plan.
///
/// # Errors
/// [`LifecycleUnavailable`] when the answer cannot be trusted — the read
/// failed, or it recognised none of a non-empty candidate set. The caller then
/// falls back to [`LifecyclePlan::reconcile_all`].
pub fn plan_lifecycle<S: BuildHasher>(
    candidates: &[Uuid],
    answer: anyhow::Result<HashMap<Uuid, RegistryLifecycle, S>>,
    unregistered: UnregisteredTenants,
) -> Result<LifecyclePlan, LifecycleUnavailable> {
    let known = answer.map_err(LifecycleUnavailable::ReadFailed)?;
    if known.is_empty() && !candidates.is_empty() {
        return Err(LifecycleUnavailable::NoneRecognised {
            candidates: candidates.len(),
        });
    }
    let mut plan = LifecyclePlan::default();
    for &tenant in candidates {
        match known.get(&tenant) {
            Some(RegistryLifecycle::Deleted) => plan.deleted.push(tenant),
            None if unregistered == UnregisteredTenants::Skip => plan.unregistered.push(tenant),
            Some(RegistryLifecycle::Live) | None => plan.reconcile.push(tenant),
        }
    }
    Ok(plan)
}

#[cfg(test)]
#[path = "tenant_lifecycle_tests.rs"]
mod tests;
