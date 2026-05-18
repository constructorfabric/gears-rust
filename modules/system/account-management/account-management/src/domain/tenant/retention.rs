//! Pure-logic retention primitives for the hard-delete pipeline.
//!
//! This module owns no I/O. The service layer (`service.rs`) drives the
//! pipeline; the repository layer (`infra/storage/repo_impl.rs`) owns the
//! SQL. What lives here is the set of algebraic helpers that both layers
//! reuse:
//!
//! * [`is_due`] — half-closed retention-window inclusion test.
//! * [`order_batch_leaf_first`] — stable leaf-first batch ordering
//!   (`depth DESC, id ASC`).
//! * [`HardDeleteOutcome`] / [`HardDeleteResult`] / [`ReaperResult`] —
//!   outcome enums + aggregate summaries emitted by the service.
//!
//! Per-tenant exponential backoff is computed by a shared helper that
//! lands together with the bootstrap saga in a later PR.

use std::time::Duration;

use modkit_macros::domain_model;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::metrics::{AM_RETENTION_INVALID_WINDOW, MetricKind, emit_metric};

/// A single tenant row selected by the retention scan.
///
/// `claimed_by` is the worker UUID stamped on the row during the
/// claim UPDATE inside `scan_retention_due`. The hard-delete pipeline
/// passes this token back into [`crate::domain::tenant::repo::TenantRepo::clear_retention_claim`]
/// so the clear only succeeds when the row is still owned by this
/// worker — a stale-claim takeover by a peer must NOT be reverted by
/// the original worker resuming after a TTL window.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantRetentionRow {
    pub id: Uuid,
    pub depth: u32,
    /// Soft-delete timestamp. Doubles as the retention-timer start —
    /// `deleted_at + retention_window` is the wall-clock at which
    /// the hard-delete sweep becomes eligible.
    pub deleted_at: OffsetDateTime,
    pub retention_window: Duration,
    pub claimed_by: Uuid,
}

/// A single tenant row claimed by the provisioning-reaper scan.
///
/// `claimed_by` is the worker UUID stamped on the row during the
/// claim UPDATE inside `scan_stuck_provisioning`. The reaper passes
/// this token back into [`crate::domain::tenant::repo::TenantRepo::clear_retention_claim`]
/// after the per-row work completes (success or failure) so a peer
/// worker can re-claim the row only after the explicit release or
/// after the same `RETENTION_CLAIM_TTL` window the retention pipeline
/// uses. The same `claimed_by` / `claimed_at` columns back both
/// pipelines — `tenants` does not need separate provisioning-claim
/// columns.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantProvisioningRow {
    pub id: Uuid,
    pub created_at: OffsetDateTime,
    pub claimed_by: Uuid,
}

/// True iff `deleted_at + retention <= now`. Comparison is inclusive —
/// a row whose effective reclaim timestamp equals `now` IS due.
#[must_use]
pub fn is_due(now: OffsetDateTime, deleted_at: OffsetDateTime, retention: Duration) -> bool {
    if time::Duration::try_from(retention).is_err() {
        emit_metric(AM_RETENTION_INVALID_WINDOW, MetricKind::Counter, &[]);
        return false;
    }

    let age = now - deleted_at;
    let Ok(elapsed) = Duration::try_from(age) else {
        return false;
    };
    elapsed >= retention
}

/// Leaf-first stable ordering for the hard-delete batch:
/// `(depth DESC, deleted_at ASC, id ASC)`.
///
/// `depth DESC` is what lets `hard_delete_one` succeed under the
/// `ON DELETE RESTRICT` parent-FK from Phase 1: children are reclaimed
/// before their parents, so the parent's in-tx child-existence guard
/// always finds the table empty when its turn arrives.
///
/// `deleted_at ASC` is the second key — it mirrors the scanner
/// contract (`scan_retention_due` returns rows in
/// `(depth DESC, deleted_at ASC, id ASC)` order to prevent starvation:
/// among siblings at the same depth, the tenant soft-deleted earliest
/// goes first). Dropping it as a tiebreaker would make the hard-delete
/// batch order siblings by `id` only — a tenant deleted last could
/// randomly beat one deleted first if its UUID sorted earlier.
/// `id ASC` is the final deterministic tiebreaker for rows deleted at
/// the exact same instant.
#[must_use]
pub fn order_batch_leaf_first(mut rows: Vec<TenantRetentionRow>) -> Vec<TenantRetentionRow> {
    rows.sort_by(|a, b| {
        b.depth
            .cmp(&a.depth)
            .then_with(|| a.deleted_at.cmp(&b.deleted_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    rows
}

/// Per-row outcome of the hard-delete pipeline for a single tenant.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum HardDeleteOutcome {
    /// Fully reclaimed — closure and tenant rows gone.
    Cleaned,
    /// A child still exists under this tenant. Defer to a later tick;
    /// children will be cleaned first thanks to leaf-first ordering.
    DeferredChildPresent,
    /// Row failed the structural eligibility guard at hard-delete time:
    /// either `status != Deleted` or `deleted_at IS NULL`.
    /// Temporal eligibility (`deleted_at + retention <= now`) is
    /// established at candidate-selection time by
    /// [`crate::domain::tenant::repo::TenantRepo::scan_retention_due`]
    /// and is not re-checked here, so this variant indicates a stale
    /// candidate set or a data-integrity anomaly rather than a
    /// retention-window extension.
    NotEligible,
    /// A cascade hook returned a retryable failure. Defer to next tick.
    CascadeRetryable,
    /// A cascade hook returned terminal failure. Skip this tenant; an
    /// operator must intervene.
    CascadeTerminal,
    /// `IdP` deprovision returned retryable failure. Defer.
    IdpRetryable,
    /// `IdP` deprovision returned terminal failure. Skip.
    IdpTerminal,
    /// `IdP` deprovision returned `UnsupportedOperation`. Treated as
    /// "nothing to do on the `IdP` side" — the pipeline continues
    /// with the DB teardown as if the deprovision succeeded, so this
    /// outcome is reported only after a successful teardown and counts
    /// toward [`HardDeleteOutcome::is_cleaned`]. The dedicated metric
    /// label (`idp_unsupported`) lets observability distinguish
    /// "cleaned via `IdP` no-op" from "cleaned via `IdP` success".
    IdpUnsupported,
    /// The DB-teardown step itself failed (storage-layer error — pool
    /// exhausted, network blip, SERIALIZABLE retry budget exhausted).
    /// Distinct from `CascadeTerminal` so the metric label and the
    /// operator's mental model don't conflate cascade-hook failures
    /// with infra failures.
    StorageError,
}

impl HardDeleteOutcome {
    /// Whether the row was reclaimed from the DB in this tick.
    /// `IdpUnsupported` counts as cleaned because the variant docstring
    /// guarantees the DB teardown ran successfully — the `IdP` no-op
    /// is reflected in the metric label, not the cleanup count.
    #[must_use]
    pub const fn is_cleaned(&self) -> bool {
        matches!(self, Self::Cleaned | Self::IdpUnsupported)
    }

    /// Whether the outcome should be counted as "deferred" (retry on
    /// a later tick with the same row still present).
    #[must_use]
    pub const fn is_deferred(&self) -> bool {
        matches!(
            self,
            Self::DeferredChildPresent | Self::CascadeRetryable | Self::IdpRetryable
        )
    }

    /// Whether the outcome should be counted as "failed" (terminal
    /// failure, tenant left in place until operator action).
    #[must_use]
    pub const fn is_failed(&self) -> bool {
        matches!(
            self,
            Self::CascadeTerminal | Self::IdpTerminal | Self::StorageError
        )
    }

    /// Stable, snake-case metric-label form of this variant. Used as
    /// the `outcome` label on `AM_TENANT_RETENTION` counter samples;
    /// kept here so the producer (service layer) does not duplicate
    /// the variant → string mapping in match arms.
    #[must_use]
    pub const fn as_metric_label(&self) -> &'static str {
        match self {
            Self::Cleaned => "cleaned",
            Self::DeferredChildPresent => "deferred_child_present",
            Self::NotEligible => "not_eligible",
            Self::CascadeRetryable => "cascade_retryable",
            Self::CascadeTerminal => "cascade_terminal",
            Self::IdpRetryable => "idp_retryable",
            Self::IdpTerminal => "idp_terminal",
            Self::IdpUnsupported => "idp_unsupported",
            Self::StorageError => "storage_error",
        }
    }
}

/// Outcome of a read-only preflight that gates `hard_delete_one`'s
/// external (cascade-hook + `IdP`) side effects on DB-side
/// preconditions. Returned by
/// [`crate::domain::tenant::repo::TenantRepo::check_hard_delete_eligibility`]
/// and consumed by the retention pipeline before any irreversible
/// `IdP` `deprovision_tenant` call. See the trait method's docstring
/// for the full rationale (closes the gap where a deferred row would
/// still tear down `IdP` state on the first attempt).
///
/// The variants intentionally mirror the relevant subset of
/// [`HardDeleteOutcome`] so the retention pipeline can short-circuit
/// with the same outcome the in-tx delete would have produced — no
/// new outcome category leaks into `HardDeleteResult` accounting.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HardDeleteEligibility {
    /// Row is in `Deleted` state, claim still ours, no live children.
    /// Caller may proceed with cascade hooks → `IdP` → in-tx delete.
    Eligible,
    /// At least one (non-deleted) child still names this tenant as
    /// parent. Maps to [`HardDeleteOutcome::DeferredChildPresent`] at
    /// the caller boundary; leaf-first scheduling will pick the child
    /// up first on a subsequent tick.
    DeferredChildPresent,
    /// Row state is no longer eligible: row is gone, status drifted
    /// (re-`Active`/`Provisioning`), `deleted_at` cleared (un-delete),
    /// or claim lost. Maps to [`HardDeleteOutcome::NotEligible`] at
    /// the caller boundary.
    NotEligible,
}

/// Aggregate summary for a single hard-delete batch tick.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HardDeleteResult {
    pub processed: u64,
    pub cleaned: u64,
    pub deferred: u64,
    pub failed: u64,
}

impl HardDeleteResult {
    /// Fold a single row outcome into the running counters.
    pub fn tally(&mut self, outcome: &HardDeleteOutcome) {
        self.processed += 1;
        if outcome.is_cleaned() {
            self.cleaned += 1;
        } else if outcome.is_deferred() {
            self.deferred += 1;
        } else if outcome.is_failed() {
            self.failed += 1;
        } else {
            // `NotEligible` is counted under `processed` only —
            // nothing happened (stale candidate set or data-integrity
            // anomaly) so it's neither cleaned nor deferred.
            // `IdpUnsupported` folds into `cleaned` via `is_cleaned()`
            // and is therefore not handled here.
        }
    }
}

/// Aggregate summary for a single reaper tick.
///
/// `compensated` counts rows where the reaper actively drove the
/// `IdP`-side teardown (clean `Ok` or `UnsupportedOperation`-mapped to
/// success). `already_absent` counts rows where the `IdP` reported the
/// tenant was already gone (`IdpDeprovisionFailure::NotFound`) — the DB
/// teardown still ran, but the operator-visible signal differs:
/// `already_absent` typically points at a lost claim or a
/// cross-system inconsistency that warrants investigation, whereas
/// `compensated` is the steady-state success path. `terminal` counts
/// rows the `IdP` plugin classified as
/// [`account_management_sdk::IdpDeprovisionFailure::Terminal`] — the
/// reaper stamps `terminal_failure_at` on the row and stops cycling
/// it through the retry loop; the operator-action-required signal is
/// emitted via this counter and the
/// `am.tenant_retention{outcome=terminal}` metric. `deferred` counts
/// the transient-defer paths (retryable `IdP` failures, infra blips,
/// and unknown variants).
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReaperResult {
    pub scanned: u64,
    pub compensated: u64,
    pub already_absent: u64,
    pub terminal: u64,
    pub deferred: u64,
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "retention_tests.rs"]
mod tests;
