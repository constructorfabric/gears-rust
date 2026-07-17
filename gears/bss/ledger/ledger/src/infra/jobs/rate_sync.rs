//! `RateSyncJob` — the periodic FX rate-sync ticker (Slice 5, Group C / design
//! §4.6). Each tick it pulls the latest published rates from the configured
//! [`RateProviderV1`] plugin in ONE round-trip and upserts them into the local
//! `ledger_fx_rate` store for every provisioned tenant, so the lock-time
//! [`RateSource`](crate::infra::fx::rate_source::RateSource) (a local read,
//! never a provider call on the posting path — C3/I-6) always resolves against
//! fresh local rows.
//!
//! ## Provider-agnostic + fail-safe
//! The provider is resolved from `ClientHub`; the default is
//! [`UnconfiguredRateProviderV1`](bss_ledger_sdk::UnconfiguredRateProviderV1)
//! (`provider_id() == "none"`), whose `fetch_latest` always errors. With no
//! adapter configured the job is INERT: it logs at debug and returns WITHOUT
//! alarming (a missing adapter is a deployment state, not a books defect). A
//! CONFIGURED provider that fails to fetch raises the `FX_SNAPSHOT_MISSING` alarm
//! (the local store goes stale → FX-needing posts block at lock time) and
//! returns — a provider outage NEVER aborts the gear.
//!
//! ## System-context / cross-tenant (mirrors `PeriodOpenJob` / `RecognitionRunJob`)
//! A provider publishes GLOBAL rates (EUR→USD is not per-tenant), but the
//! `ledger_fx_rate` store is tenant-scoped (RLS). So the job enumerates the
//! provisioned tenants from the fiscal-calendar feed (the same provisioned-LE
//! source [`PeriodOpenJob`](crate::infra::jobs::period_open) uses, under
//! [`AccessScope::allow_all`](toolkit_db::secure::AccessScope::allow_all)) and
//! upserts each fetched rate into every tenant's store via
//! [`FxRepo::upsert_rate`] (`AccessScope::for_tenant` inside the repo). A
//! per-tenant upsert fault is isolated (logged, the pass continues). The fetch
//! actor is the system-context [`SecurityContext::anonymous`] (not a per-request
//! caller).

use std::collections::BTreeSet;
use std::sync::Arc;

use bss_ledger_sdk::{ProviderRate, RateProviderError, RateProviderV1};
use toolkit_db::{DBProvider, DbError};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::infra::events::alarm_catalog;
use crate::infra::events::payloads::{AlarmCategory, LedgerInvariantAlarm};
use crate::infra::events::publisher::LedgerEventPublisher;
use crate::infra::storage::repo::{FxRepo, NewFxRate, ReferenceRepo};

/// The `ledger_fx_rate.fallback_order` stamped on a synced row. The lock-time
/// `RateSource` recomputes the effective precedence from the configured
/// `provider_order` (the stored value is only a stable secondary sort key), so a
/// freshly-synced row carries the neutral `0`.
const SYNC_FALLBACK_ORDER: i32 = 0;

/// Outcome of one rate-sync tick (returned for testability; the serve loop only
/// logs a tick error).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RateSyncReport {
    /// `true` when the configured provider fetched this tick (also `true` for an
    /// empty fetch); `false` when no adapter is configured or a configured
    /// provider failed.
    pub fetched: bool,
    /// Rates returned by the provider this tick.
    pub rates: u64,
    /// Provisioned tenants the rates were fanned out to.
    pub tenants: u64,
    /// Tenants whose upsert raised an isolated fault (logged, skipped).
    pub failed_tenants: u64,
}

/// Periodic FX rate-sync job: pull the provider's latest rates and upsert them
/// into every provisioned tenant's local store.
pub struct RateSyncJob {
    db: DBProvider<DbError>,
    provider: Arc<dyn RateProviderV1>,
    repo: FxRepo,
    publisher: Arc<LedgerEventPublisher>,
}

impl RateSyncJob {
    /// Build the job over one database provider (the provisioned-tenant
    /// enumeration), the resolved rate-provider plugin (the default
    /// `UnconfiguredRateProviderV1` when no adapter is registered), the FX repo
    /// (the upsert target), and the event publisher (the `FX_SNAPSHOT_MISSING`
    /// alarm on a configured-provider failure).
    #[must_use]
    pub fn new(
        db: DBProvider<DbError>,
        provider: Arc<dyn RateProviderV1>,
        repo: FxRepo,
        publisher: Arc<LedgerEventPublisher>,
    ) -> Self {
        Self {
            db,
            provider,
            repo,
            publisher,
        }
    }

    /// Run one rate-sync pass: fetch the provider's latest rates and fan them out
    /// to every provisioned tenant's `ledger_fx_rate` store.
    ///
    /// # Errors
    /// Returns `Err` only on an infrastructure failure enumerating the
    /// provisioned tenants (the fan-out cannot start). A provider fetch failure is
    /// NOT an error (it alarms/logs and returns `Ok` with `fetched = false`); a
    /// per-tenant upsert fault is isolated within the pass.
    pub async fn run(&self) -> anyhow::Result<RateSyncReport> {
        let ctx = SecurityContext::anonymous();
        let request_id = Uuid::now_v7().to_string();

        // ONE round-trip — `&[]` asks the adapter for its whole published table
        // (it returns everything it has, omitting pairs it cannot serve). MUST NOT
        // be called on the posting path (design C3/I-6) — only here.
        let rates = match self.provider.fetch_latest(&ctx, &[], &request_id).await {
            Ok(rates) => rates,
            Err(e) => {
                if self.provider.provider_id() == "none" {
                    // The fail-safe default (no adapter wired): a deployment state,
                    // not a books defect — log at debug, do NOT alarm.
                    tracing::debug!(
                        "bss-ledger: FX rate-sync skipped (no rate-provider adapter configured)"
                    );
                } else {
                    // A CONFIGURED provider failed: the local store goes stale, so
                    // FX-needing posts will block at lock time. Alarm + log; never
                    // abort the gear.
                    // `request_id` on both the log line and the alarm: it is the
                    // only link from this one failed tick to the adapter's
                    // per-source attempt lines, which carry the same id. The error
                    // here is a composite's LAST source's, so on its own it names
                    // neither the outage nor how many sources were tried.
                    tracing::error!(
                        provider = self.provider.provider_id(),
                        request_id = %request_id,
                        error = %e,
                        "bss-ledger: FX rate-sync provider fetch failed; raising FX_SNAPSHOT_MISSING"
                    );
                    self.emit_snapshot_missing(&ctx, self.provider.provider_id(), &request_id, &e)
                        .await;
                }
                return Ok(RateSyncReport::default());
            }
        };

        if rates.is_empty() {
            tracing::info!(
                provider = self.provider.provider_id(),
                "bss-ledger: FX rate-sync returned no rates this tick"
            );
            return Ok(RateSyncReport {
                fetched: true,
                ..RateSyncReport::default()
            });
        }

        // Fan the global rates out to every provisioned tenant's local store.
        let tenants = self.provisioned_tenants().await?;
        // The adapter's identity, for log attribution only — each stored row's
        // `provider` comes from the rate that carries it (see `upsert_tenant`).
        let adapter_id = self.provider.provider_id();
        let mut report = RateSyncReport {
            fetched: true,
            rates: u64::try_from(rates.len()).unwrap_or(u64::MAX),
            tenants: u64::try_from(tenants.len()).unwrap_or(u64::MAX),
            failed_tenants: 0,
        };
        for tenant in tenants {
            if let Err(e) = self.upsert_tenant(tenant, &rates, adapter_id).await {
                report.failed_tenants += 1;
                tracing::error!(
                    tenant_id = %tenant,
                    adapter = adapter_id,
                    error = %e,
                    "bss-ledger: FX rate upsert failed for tenant; continuing"
                );
            }
        }
        if report.failed_tenants > 0 {
            tracing::warn!(
                failed_tenants = report.failed_tenants,
                tenants = report.tenants,
                "bss-ledger: FX rate-sync tick completed with per-tenant upsert failures"
            );
        }
        Ok(report)
    }

    /// Enumerate the distinct provisioned tenants (the fiscal-calendar feed under
    /// the system-context `allow_all`, deduped) — the rate fan-out target set.
    ///
    /// # Errors
    /// Returns `Err` on an infrastructure failure reading the calendar feed.
    async fn provisioned_tenants(&self) -> anyhow::Result<BTreeSet<Uuid>> {
        let repo = ReferenceRepo::new(self.db.clone());
        let calendars = repo
            .list_all_fiscal_calendars()
            .await
            .map_err(|e| anyhow::anyhow!("rate-sync: enumerate provisioned tenants: {e}"))?;
        Ok(calendars.into_iter().map(|c| c.tenant_id).collect())
    }

    /// Upsert every fetched rate into one tenant's `ledger_fx_rate` store under
    /// the provider that published it. A whole tenant is one isolation unit (the
    /// caller logs + continues on `Err`).
    ///
    /// The stored `provider` is taken from each rate's own
    /// [`ProviderRate::provider`], not from the adapter's `provider_id()`: a
    /// composite adapter serves one whole document from whichever source
    /// answered first, so the publishing upstream varies between calls and only
    /// the rate itself knows which one it was. `adapter_id` is the adapter's
    /// constant identity and is used for log attribution only.
    async fn upsert_tenant(
        &self,
        tenant: Uuid,
        rates: &[ProviderRate],
        adapter_id: &str,
    ) -> anyhow::Result<()> {
        for rate in rates {
            // Never poison the local store with a non-positive quote: a rate
            // `<= 0` is never valid and would flip the sign of (or zero out)
            // every downstream translation. The REST ingest DTO rejects it; the
            // provider feed has no such gate, so drop the pair here (the tenant's
            // other pairs still sync) rather than upserting corrupt data.
            if rate.rate_micro <= 0 {
                tracing::warn!(
                    target: "bss-ledger.rate-sync",
                    tenant = %tenant,
                    adapter = adapter_id,
                    provider = %rate.provider,
                    base = %rate.base,
                    quote = %rate.quote,
                    rate_micro = rate.rate_micro,
                    "bss-ledger: dropping non-positive FX quote from provider feed"
                );
                continue;
            }
            self.repo
                .upsert_rate(&NewFxRate {
                    tenant_id: tenant,
                    base_currency: rate.base.clone(),
                    quote_currency: rate.quote.clone(),
                    provider: rate.provider.clone(),
                    rate_micro: rate.rate_micro,
                    as_of: rate.as_of,
                    fallback_order: SYNC_FALLBACK_ORDER,
                })
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "upsert {}->{} ({}, via adapter {adapter_id}): {e}",
                        rate.base,
                        rate.quote,
                        rate.provider
                    )
                })?;
        }
        Ok(())
    }

    /// Emit the system-scoped `FX_SNAPSHOT_MISSING` alarm on a configured-provider
    /// fetch failure. The outage is GLOBAL (the provider, not a tenant), so the
    /// alarm carries the nil tenant + an `fx-rate-sync` system scope — it signals
    /// "no tenant's rates are being refreshed". Severity is taken from the §4.7
    /// catalog (`Critical`), like the posting service's `alarm_for`.
    async fn emit_snapshot_missing(
        &self,
        ctx: &SecurityContext,
        provider_id: &str,
        request_id: &str,
        err: &RateProviderError,
    ) {
        let category = AlarmCategory::FxSnapshotMissing;
        let alarm = LedgerInvariantAlarm {
            category,
            severity: alarm_catalog::severity(category),
            tenant_id: Uuid::nil(),
            scope: "fx-rate-sync".to_owned(),
            code: category.as_str().to_owned(),
            detail: snapshot_missing_detail(provider_id, request_id, err),
            affected: vec![],
        };
        self.publisher.emit_invariant_alarm(ctx, alarm).await;
    }
}

/// Build the `FX_SNAPSHOT_MISSING` alarm detail.
///
/// Split out from [`RateSyncJob::emit_snapshot_missing`] so the wording — the
/// `request_id` in particular — is assertable without a recording publisher.
///
/// `request_id` is the load-bearing field. `provider_id` is the *composite
/// adapter's* id rather than the source that broke, and `err` is only the LAST
/// source's error, since a composite tries its sources in order and can return
/// just one error value. The individual attempts survive as the adapter's `warn`
/// lines, each stamped with this same `request_id`. Without it here, an operator
/// holding the alarm has no key to filter those lines by and cannot tell which
/// source actually failed, or whether the others failed too.
fn snapshot_missing_detail(provider_id: &str, request_id: &str, err: &RateProviderError) -> String {
    format!(
        "FX rate-sync provider '{provider_id}' fetch failed: {err}; \
         local rate store not refreshed; request_id={request_id} \
         (filter the rate-provider adapter's logs by it for the per-source attempts)"
    )
}

#[cfg(test)]
#[path = "rate_sync_tests.rs"]
mod tests;
