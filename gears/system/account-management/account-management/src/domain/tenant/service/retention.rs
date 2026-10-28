//! Retention pipeline tick on `TenantService` — `hard_delete_batch`
//! and the per-row `process_single_hard_delete` state machine that
//! invokes cascade hooks, calls
//! [`IdpPluginClient::deprovision_tenant`], and performs the
//! transactional DB teardown.
//!
//! Lives in its own submodule so the dispatch / failure-classification
//! ladder is reviewable in isolation from the CRUD methods. The hook
//! registry, `IdP` client, and config knobs are reached via crate-private
//! fields on [`TenantService`] (visible to sibling submodules of
//! `service/`).

use std::collections::BTreeMap;
use std::time::Duration;

use futures::stream::{self, StreamExt};
use time::OffsetDateTime;
use toolkit_security::AccessScope;
use tracing::warn;

use account_management_sdk::{
    IdpDeprovisionFailure, IdpDeprovisionTenantRequest, IdpTenantContext,
};

use crate::domain::metrics::{AM_DEPENDENCY_HEALTH, AM_TENANT_RETENTION, MetricKind, emit_metric};
use crate::domain::system_actor::for_retention_sweep;
use crate::domain::tenant::hooks::{HookError, TenantHardDeleteHook};
use crate::domain::tenant::repo::TenantRepo;
use crate::domain::tenant::retention::{
    HardDeleteEligibility, HardDeleteOutcome, HardDeleteResult, TenantRetentionRow,
};

use super::{CallerOrigin, TenantService};

impl<R: TenantRepo> TenantService<R> {
    /// Implements FEATURE `Hard-Delete Cleanup Sweep`.
    ///
    /// Scans retention-due rows (leaf-first), invokes registered
    /// cascade hooks, calls [`IdpPluginClient::deprovision_tenant`],
    /// and performs the transactional DB teardown via
    /// [`TenantRepo::hard_delete_one`].
    // @cpt-begin:cpt-cf-account-management-algo-tenant-hierarchy-management-hard-delete-leaf-first-scheduler:p1:inst-algo-hdel-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-hierarchy-management-hard-delete-leaf-first:p1:inst-dod-hard-delete-leaf-first
    // @cpt-begin:cpt-cf-account-management-dod-tenant-hierarchy-management-idp-tenant-deprovision:p1:inst-dod-idp-deprovision-hard-delete
    // @cpt-begin:cpt-cf-account-management-dod-tenant-hierarchy-management-data-lifecycle:p1:inst-dod-data-lifecycle-hard-delete
    #[allow(
        clippy::cognitive_complexity,
        reason = "F4 retention reaper batch loop: scan, leaf-first sort, per-row IdP deprovision + DB hard-delete with metrics emission; splitting would fragment the failure-classification ladder which must remain transactional with the per-row state machine"
    )]
    pub async fn hard_delete_batch(&self, batch_size: usize) -> HardDeleteResult {
        let now = OffsetDateTime::now_utc();
        let default_retention = Duration::from_secs(self.cfg.retention.default_window_secs);
        let system_scope = AccessScope::allow_all();
        let rows = match self
            .repo
            .scan_retention_due(&system_scope, now, default_retention, batch_size)
            .await
        {
            Ok(rows) => rows,
            Err(err) => {
                warn!(
                    target: "am.retention",
                    error = %err,
                    "hard_delete_batch: scan failed; skipping tick"
                );
                // Distinguish a healthy idle tick (zero due rows) from
                // a tick that no-op'd because the scan itself faulted
                // (DB contention, transient SeaORM error, …).
                // `scan_retention_due` does not go through
                // `with_serializable_retry`, so the otherwise-shared
                // `AM_SERIALIZABLE_RETRY{outcome=exhausted}` signal
                // does not cover this path.
                emit_metric(
                    AM_TENANT_RETENTION,
                    MetricKind::Counter,
                    &[("retention_job", "hard_delete"), ("outcome", "scan_failed")],
                );
                return HardDeleteResult::default();
            }
        };

        // Bucket the batch by depth. Within a single depth bucket
        // sibling tenants share no FK ordering constraint and can be
        // reclaimed concurrently. Buckets are processed leaf-first
        // (deepest depth → root) so the parent FK guard always sees
        // child rows already gone by the time the parent's turn arrives.
        let mut by_depth: BTreeMap<u32, Vec<TenantRetentionRow>> = BTreeMap::new();
        for row in rows {
            by_depth.entry(row.depth).or_default().push(row);
        }

        // Snapshot hooks once per tick so the per-tenant pipeline does
        // not re-clone the registration `Vec` for every row.
        let hooks_snapshot: Vec<TenantHardDeleteHook> = {
            let guard = self.hooks.lock();
            guard.clone()
        };

        // .max(1) is defense-in-depth for hand-built TenantService instances that bypass validate(); buffer_unordered(0) would stall.
        let concurrency = self.cfg.retention.hard_delete_concurrency.max(1);
        let mut result = HardDeleteResult::default();
        // `BTreeMap` iterates keys ascending; reverse to drain the
        // deepest bucket first.
        //
        // Within each bucket: stream-and-process. The shape is
        // `buffer_unordered(concurrency)` for `process_single_hard_delete`
        // (cascade hooks → IdP → DB teardown), then
        // `while let Some(...) = stream.next().await` applies parking
        // and claim-release per row AS IT ARRIVES rather than after
        // the bucket's slowest row finishes. Streaming matters here
        // for the same reason as in the reaper:
        //   * one slow / hung row would otherwise hold every other
        //     completed row's claim past `RETENTION_CLAIM_TTL`
        //     (~10 min), letting a peer worker re-claim and re-run
        //     cascade hooks AND `deprovision_tenant` against the
        //     same tenant — cascade hooks have side effects on
        //     sibling-feature rows, so a duplicate run is not free;
        //   * if the slow row's future never returns, every other
        //     row in the bucket would block indefinitely.
        // Per-row state (metric emission, parking call, claim-release
        // call, `result.tally`) stays inside the loop body, so the
        // serial `&mut result` borrow is preserved — DB writes still
        // serialise on the `tenants` write path. Cross-bucket
        // ordering is preserved by the outer `for`: depth N is fully
        // drained (including its slow tail) before depth N-1 begins,
        // which is required because parents at N-1 would otherwise
        // see still-present children at N and defer.
        //
        // `&self` is captured shared across concurrent
        // `process_single_hard_delete` calls. Safe: `TenantService<R>`
        // is `Sync` (all fields are `Sync` — `Arc<R>`, `Arc<idp>`,
        // plain-value config, `parking_lot::Mutex`-guarded hooks);
        // `process_single_hard_delete` mutates only DB / IdP / metric
        // state, not service state. Do NOT "fix" by cloning into
        // each future or splitting handles — both would lose the
        // streaming property without a corresponding correctness
        // gain.
        for (_depth, bucket) in by_depth.into_iter().rev() {
            let mut stream = stream::iter(bucket)
                .map(|row| {
                    let hooks = hooks_snapshot.as_slice();
                    async move {
                        let id = row.id;
                        let depth = row.depth;
                        let claimed_by = row.claimed_by;
                        let outcome = self.process_single_hard_delete(row, hooks).await;
                        (id, depth, claimed_by, outcome)
                    }
                })
                .buffer_unordered(concurrency);

            while let Some((id, depth, claimed_by, outcome)) = stream.next().await {
                if outcome.is_cleaned() {
                    // TODO(events): emit AM event when platform event-bus lands.
                    // `is_cleaned()` covers both `Cleaned` and
                    // `IdpUnsupported`: per the variant docstring,
                    // `IdpUnsupported` is reported only after a
                    // successful DB teardown, so the cleanup-completed
                    // event applies. The metric label
                    // (`outcome.as_metric_label()` below) still
                    // distinguishes "cleaned via IdP success" from
                    // "cleaned via IdP no-op".
                    tracing::info!(
                        target: "am.events",
                        kind = "hardDeleteCleanupCompleted",
                        actor = "system",
                        tenant_id = %id,
                        depth = depth,
                        "am tenant state changed"
                    );
                }
                emit_metric(
                    AM_TENANT_RETENTION,
                    MetricKind::Counter,
                    &[
                        ("retention_job", "hard_delete"),
                        ("outcome", outcome.as_metric_label()),
                    ],
                );

                // Park terminal-class outcomes via
                // `terminal_failure_at` so the scanner stops re-
                // attempting the same broken row every tick. The
                // retention pipeline classified the row as
                // operator-action-required (panicking / `Terminal`
                // cascade hook, or IdP returned
                // `IdpDeprovisionFailure::Terminal`); without parking,
                // a permanently buggy hook or vendor-side state
                // would cause the row to churn the scanner /
                // hook-stack / IdP indefinitely (one tick per
                // `tick_secs`, default 60s) with no progress and
                // pure observability noise. Symmetric to the
                // reaper-side stamp on `Provisioning` rows
                // classified as `IdpTerminal`. Operator clears
                // `terminal_failure_at` (manual SQL) once the
                // underlying issue is fixed; the next scan picks
                // the row back up. `StorageError` is intentionally
                // NOT parked — it is a transient infra fault, not a
                // terminal classification.
                if matches!(
                    outcome,
                    HardDeleteOutcome::CascadeTerminal | HardDeleteOutcome::IdpTerminal
                ) {
                    match self
                        .repo
                        .mark_retention_terminal_failure(
                            &AccessScope::allow_all(),
                            id,
                            claimed_by,
                            OffsetDateTime::now_utc(),
                        )
                        .await
                    {
                        // Ok(true) parked; Ok(false) idempotent no-op (lost claim or already parked). No separate metric needed.
                        Ok(_) => {}
                        Err(err) => {
                            warn!(
                                target: "am.retention",
                                tenant_id = %id,
                                error = %err,
                                "failed to mark terminal_failure_at after terminal-class \
                                 outcome; row will re-fail next tick until parking lands"
                            );
                            // Distinct outcome label so the dashboard
                            // separates a parking-machinery infra
                            // fault from a hook / IdP terminal
                            // classification.
                            emit_metric(
                                AM_TENANT_RETENTION,
                                MetricKind::Counter,
                                &[("retention_job", "hard_delete"), ("outcome", "park_failed")],
                            );
                        }
                    }
                }

                // Hold the claim on `DeferredChildPresent` so the
                // scanner's `LIMIT` window is not monopolized by a
                // backlog of blocked parents on the very next tick.
                // The retention claim ages out via
                // `RETENTION_CLAIM_TTL` (~10 min), giving a
                // deterministic back-off before the row is re-picked.
                // By that time the still-undue child row has either
                // become due (and will be processed leaf-first first,
                // unblocking the parent), or it is still undue and
                // the parent simply waits another TTL — without
                // starving shallower eligible rows in between. Other
                // non-cleaned outcomes (`StorageError`,
                // `NotEligible`, `IdpRetryable`, …) still clear
                // promptly so the row can be re-attempted on the
                // next tick.
                let release_claim_now =
                    !outcome.is_cleaned() && outcome != HardDeleteOutcome::DeferredChildPresent;
                if release_claim_now
                    && let Err(err) = self
                        .repo
                        .clear_retention_claim(&AccessScope::allow_all(), id, claimed_by)
                        .await
                {
                    warn!(
                        target: "am.retention",
                        tenant_id = %id,
                        error = %err,
                        "failed to clear retention claim after non-cleaned outcome"
                    );
                    // Sustained failures keep rows unavailable for
                    // re-claim until `RETENTION_CLAIM_TTL` ages them
                    // out (~10 min). Emit a dedicated counter so the
                    // dashboard separates a healthy claim release
                    // from a storage-fault-induced stale-claim cliff.
                    emit_metric(
                        AM_TENANT_RETENTION,
                        MetricKind::Counter,
                        &[
                            ("retention_job", "hard_delete"),
                            ("outcome", "claim_clear_failed"),
                        ],
                    );
                }
                result.tally(&outcome);
            }
        }
        result
    }
    // @cpt-end:cpt-cf-account-management-dod-tenant-hierarchy-management-data-lifecycle:p1:inst-dod-data-lifecycle-hard-delete
    // @cpt-end:cpt-cf-account-management-dod-tenant-hierarchy-management-idp-tenant-deprovision:p1:inst-dod-idp-deprovision-hard-delete
    // @cpt-end:cpt-cf-account-management-dod-tenant-hierarchy-management-hard-delete-leaf-first:p1:inst-dod-hard-delete-leaf-first
    // @cpt-end:cpt-cf-account-management-algo-tenant-hierarchy-management-hard-delete-leaf-first-scheduler:p1:inst-algo-hdel-service

    #[allow(
        clippy::cognitive_complexity,
        reason = "single linear pipeline: hooks -> idp -> db teardown; splitting obscures the flow"
    )]
    async fn process_single_hard_delete(
        &self,
        row: TenantRetentionRow,
        hooks: &[TenantHardDeleteHook],
    ) -> HardDeleteOutcome {
        // 0. Preflight eligibility check — DB-side preconditions
        //    verified BEFORE any external (cascade-hook / IdP) side
        //    effect runs. Without this gate, a row that is in fact
        //    deferred (parent with live child, status drifted, claim
        //    lost) would still trigger an irreversible
        //    `IdpPluginClient::deprovision_tenant` call —
        //    leaving IdP-side state torn down while AM keeps the row.
        //    The check is read-only and racy; `hard_delete_one`'s
        //    in-tx defense-in-depth still catches a lost race, and
        //    `IdpDeprovisionFailure::NotFound → IdpUnsupported` recovers
        //    on next tick. See `TenantRepo::check_hard_delete_eligibility`
        //    docstring for the full rationale.
        match self
            .repo
            .check_hard_delete_eligibility(&AccessScope::allow_all(), row.id, row.claimed_by)
            .await
        {
            Ok(HardDeleteEligibility::Eligible) => {}
            Ok(HardDeleteEligibility::DeferredChildPresent) => {
                return HardDeleteOutcome::DeferredChildPresent;
            }
            Ok(HardDeleteEligibility::NotEligible) => {
                return HardDeleteOutcome::NotEligible;
            }
            Err(err) => {
                warn!(
                    target: "am.retention",
                    tenant_id = %row.id,
                    error = %err,
                    "hard_delete preflight failed; routing to StorageError outcome"
                );
                return HardDeleteOutcome::StorageError;
            }
        }

        // 1. Cascade hooks — run all, surface the strongest non-ok outcome.
        let mut strongest: Option<HookError> = None;
        for hook in hooks {
            let fut = hook(row.id);
            // Spawn into its own task so a panicking hook cannot kill
            // the retention loop. A panic is operator-action-required
            // (the dominant case is a deterministic bug in the hook
            // impl, where retrying just panics again forever), so
            // surface as `Terminal` rather than `Retryable`. The
            // resulting `CascadeTerminal` outcome is then parked via
            // `mark_retention_terminal_failure` in the outer loop
            // (symmetric to the reaper's stamping for IdP-terminal
            // failures on `Provisioning` rows), so the row drops out
            // of the scanner until an operator clears
            // `terminal_failure_at`. `JoinError::is_cancelled` is
            // unreachable here because we never call `.abort()` on
            // the spawned handle (that is the sole trigger for the
            // cancelled variant); we only hold the handle across
            // the immediate `.await`. Panic is therefore the only
            // meaningful case.
            let result = tokio::spawn(fut).await.unwrap_or_else(|e| {
                Err(HookError::Terminal {
                    detail: format!("hook panicked: {e}"),
                })
            });
            match result {
                Ok(()) => {}
                Err(HookError::Retryable { detail }) => {
                    let combined = match strongest {
                        Some(prev @ HookError::Terminal { .. }) => prev,
                        _ => HookError::Retryable { detail },
                    };
                    strongest = Some(combined);
                }
                Err(HookError::Terminal { detail }) => {
                    strongest = Some(HookError::Terminal { detail });
                }
            }
        }
        if let Some(err) = strongest {
            match err {
                HookError::Retryable { detail } => {
                    warn!(
                        target: "am.retention",
                        tenant_id = %row.id,
                        detail,
                        "hard_delete deferred by retryable cascade hook"
                    );
                    return HardDeleteOutcome::CascadeRetryable;
                }
                HookError::Terminal { detail } => {
                    warn!(
                        target: "am.retention",
                        tenant_id = %row.id,
                        detail,
                        "hard_delete skipped by terminal cascade hook"
                    );
                    return HardDeleteOutcome::CascadeTerminal;
                }
            }
        }

        // 2. IdP deprovision — outside any TX.
        //
        // The match returns `idp_skipped: bool` to signal whether the
        // IdP step was a no-op (`UnsupportedOperation` / `NotFound`).
        // `idp_skipped == true` plus a successful DB teardown produces
        // [`HardDeleteOutcome::IdpUnsupported`] (which `is_cleaned()`
        // covers) so the `am.tenant_retention` counter emits the
        // dedicated `idp_unsupported` label and dashboards can
        // distinguish "cleaned via IdP success" from "cleaned via IdP
        // no-op". Without this propagation the variant docstring
        // (`reported only after a successful teardown and counts
        // toward is_cleaned`) would be unreachable.
        let tenant_context = match self
            .load_tenant_context(CallerOrigin::Background, row.id)
            .await
        {
            Ok(ctx) => ctx,
            Err(err) => {
                // Could not assemble the context (registry blip or
                // tenant row vanished mid-tick). The plugin contract
                // requires a typed `tenant_type` on every call, so
                // we cannot proceed; the IdP step is effectively
                // blocked on an upstream dependency. `IdpRetryable`
                // is the closest documented outcome — same metric
                // family the retention loop already alerts on for
                // "IdP step needs another tick", and it routes
                // through the existing `is_deferred` accounting so
                // the claim releases cleanly.
                warn!(
                    target: "am.retention",
                    tenant_id = %row.id,
                    error = %err,
                    "hard_delete deferred: failed to assemble TenantContext for deprovision_tenant"
                );
                return HardDeleteOutcome::IdpRetryable;
            }
        };
        let sys_ctx = for_retention_sweep(row.id);
        let idp_skipped = match self
            .idp
            .deprovision_tenant(
                &sys_ctx,
                &IdpDeprovisionTenantRequest::new(IdpTenantContext::from(&tenant_context)),
            )
            .await
        {
            Ok(()) => {
                emit_metric(
                    AM_DEPENDENCY_HEALTH,
                    MetricKind::Counter,
                    &[
                        ("target", "idp"),
                        ("op", "deprovision_tenant"),
                        ("outcome", "success"),
                    ],
                );
                false
            }
            Err(failure) => {
                emit_metric(
                    AM_DEPENDENCY_HEALTH,
                    MetricKind::Counter,
                    &[
                        ("target", "idp"),
                        ("op", "deprovision_tenant"),
                        ("outcome", failure.as_metric_label()),
                    ],
                );
                match failure {
                    // Vendor SDK detail strings may carry hostnames,
                    // endpoint paths, or token-bearing fragments — the
                    // same class of secrets the `am.idp` mapping in
                    // `domain/idp` redacts. The retention path logs
                    // into the long-retention `am.retention` target, so
                    // the raw text MUST be redacted here too (matches
                    // the provisioning-reaper contract: FNV-1a digest +
                    // character length for operator correlation).
                    IdpDeprovisionFailure::Retryable { detail } => {
                        let (digest, len) = crate::domain::idp::redact_provider_detail(&detail);
                        warn!(
                            target: "am.retention",
                            tenant_id = %row.id,
                            provider_detail_digest = digest,
                            provider_detail_len = len,
                            "hard_delete deferred by retryable IdP failure (raw detail redacted; correlate via digest)"
                        );
                        return HardDeleteOutcome::IdpRetryable;
                    }
                    IdpDeprovisionFailure::Terminal { detail } => {
                        let (digest, len) = crate::domain::idp::redact_provider_detail(&detail);
                        warn!(
                            target: "am.retention",
                            tenant_id = %row.id,
                            provider_detail_digest = digest,
                            provider_detail_len = len,
                            "hard_delete skipped by terminal IdP failure (raw detail redacted; correlate via digest)"
                        );
                        return HardDeleteOutcome::IdpTerminal;
                    }
                    IdpDeprovisionFailure::UnsupportedOperation { .. } => {
                        // `UnsupportedOperation` is **only** safe to
                        // treat as "skip IdP, continue local teardown"
                        // when the deployment has explicitly opted
                        // out of an IdP (`cfg.idp.required = false` →
                        // wired to `NoopIdpProvider`). When a real
                        // plugin returns this, the vendor is
                        // signalling that it does not support
                        // deprovision but external state may exist —
                        // hard-deleting the AM row would orphan that
                        // state. Defer to the next tick instead so an
                        // operator (or a redeployed plugin that
                        // implements deprovision) can resolve it.
                        if self.cfg.idp.required {
                            warn!(
                                target: "am.retention",
                                tenant_id = %row.id,
                                "hard_delete deferred: IdP plugin returned UnsupportedOperation but \
                                 idp.required=true; refusing to orphan vendor-side state"
                            );
                            return HardDeleteOutcome::IdpRetryable;
                        }
                        // No-IdP deployment: safe to skip the IdP
                        // step and continue with DB teardown. Falls
                        // through to `Cleaned → IdpUnsupported`
                        // translation below.
                        true
                    }
                    IdpDeprovisionFailure::NotFound { .. } => {
                        // Vendor reports the tenant is already gone
                        // (possibly from a previous attempt that lost
                        // its claim post-call). Always success-
                        // equivalent, regardless of `cfg.idp.required`
                        // — there is nothing left to orphan.
                        true
                    }
                    // `IdpDeprovisionFailure` is `#[non_exhaustive]`; the
                    // wildcard guards against a future SDK variant
                    // landing without a service-side classification.
                    #[allow(unreachable_patterns, reason = "non_exhaustive enum forward-compat")]
                    _ => {
                        warn!(
                            target: "am.retention",
                            tenant_id = %row.id,
                            "hard_delete: unknown IdpDeprovisionFailure variant; deferring as retryable"
                        );
                        return HardDeleteOutcome::IdpRetryable;
                    }
                }
            }
        };

        // 3. DB teardown — fenced on the same `claimed_by` token the
        //    preflight verified, so a peer that re-claimed the row
        //    during the hooks/IdP window short-circuits to
        //    `NotEligible` instead of double-tearing-down state.
        match self
            .repo
            .hard_delete_one(&AccessScope::allow_all(), row.id, row.claimed_by)
            .await
        {
            // Translate `Cleaned` → `IdpUnsupported` when the IdP
            // step was a no-op. `is_cleaned()` covers both, so
            // downstream consumers (claim-clear skip,
            // `hardDeleteCleanupCompleted` event) treat them the
            // same; only the metric label differs.
            Ok(HardDeleteOutcome::Cleaned) if idp_skipped => HardDeleteOutcome::IdpUnsupported,
            Ok(outcome) => outcome,
            Err(err) => {
                // Storage-layer fault — pool exhausted, SERIALIZABLE
                // retry budget exhausted, network blip. Routed to a
                // dedicated `StorageError` outcome so the
                // `am.tenant_retention` counter does not lump infra
                // faults under `cascade_terminal` (which is meant for
                // user-supplied hook failures).
                warn!(
                    target: "am.retention",
                    tenant_id = %row.id,
                    error = %err,
                    "hard_delete db teardown failed"
                );
                HardDeleteOutcome::StorageError
            }
        }
    }
}
