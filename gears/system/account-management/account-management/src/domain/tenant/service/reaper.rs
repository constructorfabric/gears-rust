//! Provisioning-reaper tick on `TenantService` —
//! `reap_stuck_provisioning`. Cleanup is performed **directly**
//! against the `tenants` row: a successful (or success-equivalent)
//! `IdpPluginClient::deprovision_tenant` call is followed
//! by an immediate hard-delete via `repo.compensate_provisioning()`,
//! bypassing the soft-delete + retention pipeline entirely (see
//! `compensate_provisioning_row`). This keeps stuck-`Provisioning`
//! cleanup to a single tick of latency and avoids leaking transient
//! tombstones into the retention scanner. Terminal classifications
//! stamp `terminal_failure_at` (parking the row out of the retry
//! loop) and defer/retryable arms explicitly release the row's
//! `tenants.claimed_by` / `claimed_at` lease so a peer worker may
//! pick it up on the next tick.
//!
//! Each tick claims a batch of stuck `Provisioning` rows via the
//! same lease that backs the retention pipeline, so two replicas
//! cannot stamp duplicate `deprovision_tenant` calls onto the same
//! row inside one `RETENTION_CLAIM_TTL` window. Defense-in-depth
//! against the
//! [`account_management_sdk::IdpDeprovisionFailure::NotFound`]-
//! as-success-equivalent error mapping (which handles edge cases —
//! crash recovery, stale claim takeover).
//!
//! Retry / backoff / circuit-breaker policy is owned by the
//! [`account_management_sdk::IdpPluginClient`]
//! implementation — a `Retryable` return signals that the plugin
//! has exhausted its own retry budget for that call, and AM simply
//! defers the row to the next reaper tick (default 30 s).

use std::time::Duration;

use futures::stream::{self, StreamExt};
use time::OffsetDateTime;
use toolkit_macros::domain_model;
use toolkit_security::AccessScope;
use tracing::warn;

use account_management_sdk::{
    IdpDeprovisionFailure, IdpDeprovisionTenantRequest, IdpTenantContext,
};

use crate::domain::metrics::{
    AM_TENANT_CLOSURE_ROWS, AM_TENANT_RETENTION, AM_TENANTS, MetricKind, emit_gauge_value,
    emit_metric,
};
use crate::domain::system_actor::for_provisioning_reaper;
use crate::domain::tenant::repo::TenantRepo;
use crate::domain::tenant::retention::ReaperResult;

use super::{CallerOrigin, TenantService};

/// Compensation-arm classification for a single reaper row. Lets
/// the per-row body in `reap_stuck_provisioning` decide whether to
/// proceed with the local DB teardown without re-matching the full
/// `IdpDeprovisionFailure` shape twice.
#[domain_model]
enum ReaperOutcome {
    /// Plugin acknowledged the deprovision (or there was nothing
    /// `IdP`-side to do); proceed to DB teardown. The label is the
    /// metric `outcome=` value emitted on success.
    Compensable(&'static str),
    /// `IdP` plugin classified the deprovision as non-recoverable
    /// (`IdpDeprovisionFailure::Terminal`). Stamp `terminal_failure_at`
    /// on the row so `scan_stuck_provisioning` filters it out of the
    /// retry loop until an operator intervenes. Distinct from
    /// [`Self::Defer`]: a deferred row goes back on the next tick;
    /// a terminal row stays stamped until manually cleared.
    Terminal,
    /// Defer the row to the next tick; metric label + log detail
    /// already emitted by the caller. The claim is released on the
    /// way out so a peer worker may pick the row up.
    Defer,
}

impl<R: TenantRepo> TenantService<R> {
    /// Refresh the `am_tenants` inventory gauge with a platform-wide
    /// count per `(status, self_managed)`. Driven off the reaper tick
    /// (a periodic loop that always runs) so the gauge stays current
    /// without a dedicated scheduler. Best-effort: a count failure is
    /// logged and the previous gauge sample is left in place rather
    /// than failing the tick.
    pub async fn refresh_tenant_inventory(&self) {
        let scope = AccessScope::allow_all();
        self.refresh_tenant_status_gauge(&scope).await;
        self.refresh_closure_gauge(&scope).await;
    }

    /// Emit `am_tenants{status,self_managed}` from a platform-wide count.
    async fn refresh_tenant_status_gauge(&self, scope: &AccessScope) {
        match self.repo.count_tenants_by_status(scope).await {
            Ok(rows) => {
                for (status, self_managed, count) in rows {
                    emit_gauge_value(
                        AM_TENANTS,
                        i64::try_from(count).unwrap_or(i64::MAX),
                        &[
                            ("status", status.as_str()),
                            ("self_managed", if self_managed { "true" } else { "false" }),
                        ],
                    );
                }
            }
            Err(err) => {
                warn!(
                    target: "am.lifecycle",
                    error = %err,
                    "refresh_tenant_inventory: count query failed; am_tenants gauge not refreshed this tick"
                );
            }
        }
    }

    /// Emit `am_tenant_closure_rows` (the `tenant_closure` table size).
    async fn refresh_closure_gauge(&self, scope: &AccessScope) {
        match self.repo.count_closure_rows(scope).await {
            Ok(rows) => {
                emit_gauge_value(
                    AM_TENANT_CLOSURE_ROWS,
                    i64::try_from(rows).unwrap_or(i64::MAX),
                    &[],
                );
            }
            Err(err) => {
                warn!(
                    target: "am.lifecycle",
                    error = %err,
                    "refresh_tenant_inventory: closure-count query failed; am_tenant_closure_rows gauge not refreshed this tick"
                );
            }
        }
    }

    /// Implements FEATURE `Provisioning Reaper`.
    ///
    /// Per-row pipeline: classify the `IdP` deprovision response, then
    /// either (a) hard-delete the row directly via
    /// `repo.compensate_provisioning()` (compensable / already-absent
    /// arms), (b) stamp `terminal_failure_at` and park the row
    /// (`Terminal` arm), or (c) release the claim and defer to the
    /// next tick (`Retryable` / unknown-variant arms). A storage fault
    /// on the (a) hard-delete falls through to (c) — defer + release —
    /// so a peer reaper can retry the compensation on a later tick.
    /// See `compensate_provisioning_row` for the storage-fault → defer
    /// branch that fences the hard-delete on the worker's claim and
    /// releases it on infra failure.
    /// Worst-case stuck → row-gone latency is one `reaper_tick_secs`
    /// (no retention-pipeline hop). See FEATURE §3
    /// `algo-tenant-hierarchy-management-provisioning-reaper-compensation`.
    #[allow(
        clippy::cognitive_complexity,
        reason = "single linear pipeline: scan -> per-row classification + claim release"
    )]
    // @cpt-begin:cpt-cf-account-management-algo-tenant-hierarchy-management-provisioning-reaper-compensation:p1:inst-algo-reap-service
    // @cpt-begin:cpt-cf-account-management-dod-tenant-hierarchy-management-idp-tenant-provisioning-failure:p1:inst-dod-idp-provisioning-failure-reaper
    // @cpt-begin:cpt-cf-account-management-dod-tenant-hierarchy-management-idp-tenant-deprovision:p1:inst-dod-idp-deprovision-reaper
    // @cpt-begin:cpt-cf-account-management-dod-tenant-hierarchy-management-data-remediation:p2:inst-dod-data-remediation-reaper
    pub async fn reap_stuck_provisioning(&self, threshold: Duration) -> ReaperResult {
        let now = OffsetDateTime::now_utc();
        let older_than = match time::Duration::try_from(threshold) {
            Ok(d) => now - d,
            Err(err) => {
                // `time::Duration::try_from(std::time::Duration)` only
                // refuses values past `i64::MAX` seconds (~292 yrs); any
                // realistic misconfig of `provisioning_timeout_secs`
                // lands here and would otherwise look like a clean
                // empty tick. Surface it loudly so a bad config does
                // not masquerade as "nothing stuck."
                warn!(
                    target: "am.retention",
                    threshold_secs = threshold.as_secs(),
                    error = %err,
                    "reap_stuck_provisioning: threshold exceeds time::Duration range; skipping tick"
                );
                return ReaperResult::default();
            }
        };
        let system_scope = AccessScope::allow_all();
        let rows = match self
            .repo
            .scan_stuck_provisioning(&system_scope, now, older_than, self.cfg.reaper.batch_size)
            .await
        {
            Ok(rows) => rows,
            Err(err) => {
                warn!(
                    target: "am.retention",
                    error = %err,
                    "reap_stuck_provisioning: scan failed; skipping tick"
                );
                // Distinguish a healthy idle tick (zero stuck rows)
                // from a tick that no-op'd because the scan faulted.
                // Mirrors the symmetric signal in
                // `hard_delete_batch`.
                emit_metric(
                    AM_TENANT_RETENTION,
                    MetricKind::Counter,
                    &[
                        ("retention_job", "provisioning_reaper"),
                        ("outcome", "scan_failed"),
                    ],
                );
                return ReaperResult::default();
            }
        };

        let mut result = ReaperResult {
            scanned: u64::try_from(rows.len()).unwrap_or(u64::MAX),
            ..ReaperResult::default()
        };

        // IdP classify fan-out + DB action streaming. The
        // `IdP::deprovision_tenant` call is the dominant per-row cost
        // (full provider round-trip, hundreds of ms typical,
        // multi-second on degraded providers); a sequential
        // `for row in rows { … .await … }` would scale one tick as
        // `batch_size × IdP_RTT` and risk slipping past
        // `reaper.tick_secs`.
        //
        // `buffer_unordered(concurrency)` polls up to
        // `deprovision_concurrency` IdP classifications in parallel
        // within this task, and the `while let Some(...) = stream.next()`
        // shape applies each row's DB action AS IT ARRIVES rather
        // than after `collect()` waits for the slowest classification.
        // Streaming matters here because:
        //   * one slow / hung IdP future would otherwise hold every
        //     completed row's claim past `RETENTION_CLAIM_TTL` (~10 min),
        //     letting a peer reaper re-claim and issue a duplicate
        //     `deprovision_tenant` against the same tenant; and
        //   * if the slow future never returns at all, every other row
        //     in the batch would block indefinitely.
        // DB writes themselves stay sequential — the `while let` body
        // awaits each `compensate_provisioning_row` /
        // `mark_terminal_provisioning_row` / `release_claim` before
        // pulling the next stream item — which keeps `result` mutable-
        // borrow-safe and avoids per-row contention on the `tenants`
        // write path (DB write is 10–100× faster than the IdP RTT it
        // serves, so this serialisation is invisible at the tick
        // budget).
        //
        // We capture `classified_at` inside the per-row future so
        // `terminal_failure_at` reflects the actual IdP-observation
        // moment regardless of how long the row sits in the stream
        // queue waiting for its DB-write turn.
        //
        // The `async move { self.classify_deprovision(id).await … }`
        // closure intentionally captures `&self` shared across
        // concurrent futures (every `TenantService<R: TenantRepo>` is
        // `Sync` because all its fields — `repo: Arc<R>`, `idp: Arc<…>`,
        // `cfg: AccountManagementConfig` (a plain value type with no
        // interior mutability — `derive(Default, Deserialize)` over
        // numeric / boolean knobs, no `Cell` / `RefCell` / `UnsafeCell`
        // fields), `enforcer: Arc<…>`, and the
        // `parking_lot::Mutex`-guarded hooks — are themselves `Sync`).
        // `classify_deprovision` is read-only over `self.idp` /
        // `self.cfg` and emits side-effect-free metrics, so concurrent
        // shared access is safe. Do NOT "fix" this by cloning into
        // each future or splitting handles — both would lose the
        // streaming property without a corresponding correctness gain.
        // If a future change introduces an interior-mutable field on
        // `AccountManagementConfig`, this `&self` capture pattern
        // needs a re-audit (an analogous comment lives next to the
        // retention pipeline's symmetric streaming refactor).
        //
        // .max(1) is defense-in-depth for hand-built TenantService instances that bypass validate(); buffer_unordered(0) would stall.
        let concurrency = self.cfg.reaper.deprovision_concurrency.max(1);
        let mut stream = stream::iter(rows)
            .map(|row| {
                let id = row.id;
                let claimed_by = row.claimed_by;
                async move {
                    let outcome = self.classify_deprovision(id).await;
                    let classified_at = OffsetDateTime::now_utc();
                    (id, claimed_by, outcome, classified_at)
                }
            })
            .buffer_unordered(concurrency);

        while let Some((id, claimed_by, outcome, classified_at)) = stream.next().await {
            match outcome {
                ReaperOutcome::Compensable(label) => {
                    self.compensate_provisioning_row(
                        &system_scope,
                        id,
                        claimed_by,
                        label,
                        &mut result,
                    )
                    .await;
                }
                ReaperOutcome::Terminal => {
                    self.mark_terminal_provisioning_row(
                        &system_scope,
                        id,
                        claimed_by,
                        classified_at,
                        &mut result,
                    )
                    .await;
                }
                ReaperOutcome::Defer => {
                    result.deferred += 1;
                    self.release_claim(&system_scope, id, claimed_by).await;
                }
            }
        }

        result
    }
    // @cpt-end:cpt-cf-account-management-dod-tenant-hierarchy-management-data-remediation:p2:inst-dod-data-remediation-reaper
    // @cpt-end:cpt-cf-account-management-dod-tenant-hierarchy-management-idp-tenant-deprovision:p1:inst-dod-idp-deprovision-reaper
    // @cpt-end:cpt-cf-account-management-dod-tenant-hierarchy-management-idp-tenant-provisioning-failure:p1:inst-dod-idp-provisioning-failure-reaper
    // @cpt-end:cpt-cf-account-management-algo-tenant-hierarchy-management-provisioning-reaper-compensation:p1:inst-algo-reap-service

    /// Call the `IdP` and translate the response into a
    /// [`ReaperOutcome`]. Side effects (`warn!` + `emit_metric` for
    /// non-success branches) are performed here so the caller body
    /// stays linear.
    #[allow(
        clippy::cognitive_complexity,
        reason = "flat dispatch over five Deprovision outcomes; splitting hides the per-branch label/log"
    )]
    #[allow(
        unreachable_patterns,
        reason = "IdpDeprovisionFailure is #[non_exhaustive]; the wildcard guards against future SDK variants"
    )]
    async fn classify_deprovision(&self, tenant_id: uuid::Uuid) -> ReaperOutcome {
        let tenant_context = match self
            .load_tenant_context(CallerOrigin::Background, tenant_id)
            .await
        {
            Ok(ctx) => ctx,
            Err(err) => {
                // Could not assemble the context — typically a
                // types-registry blip or a missing row.
                // Deprovision_tenant cannot proceed without a typed
                // tenant_type per the SDK contract, so defer the
                // row to the next tick and release the claim. A
                // peer (or this worker on retry) will try again
                // once the registry recovers.
                warn!(
                    target: "am.retention",
                    tenant_id = %tenant_id,
                    error = %err,
                    "reaper: failed to assemble TenantContext for deprovision_tenant; deferring row"
                );
                emit_metric(
                    AM_TENANT_RETENTION,
                    MetricKind::Counter,
                    &[
                        ("retention_job", "provisioning_reaper"),
                        ("outcome", "context_load_failed"),
                    ],
                );
                return ReaperOutcome::Defer;
            }
        };
        let sys_ctx = for_provisioning_reaper(tenant_id);
        match self
            .idp
            .deprovision_tenant(
                &sys_ctx,
                &IdpDeprovisionTenantRequest::new(IdpTenantContext::from(&tenant_context)),
            )
            .await
        {
            Ok(()) => ReaperOutcome::Compensable("compensated"),
            Err(IdpDeprovisionFailure::UnsupportedOperation { .. }) => {
                // `UnsupportedOperation` is only safe to treat as
                // compensable when the deployment opted out of an
                // IdP entirely (`cfg.idp.required = false` → wired
                // to `NoopIdpProvider`). A real plugin returning
                // this is signalling that it cannot perform
                // deprovision but external state may exist — hard-
                // deleting the AM row would orphan that vendor-side
                // state with no local repair handle. Park the row
                // terminal so an operator (or a redeployed plugin
                // that implements deprovision) can resolve it.
                if self.cfg.idp.required {
                    warn!(
                        target: "am.retention",
                        tenant_id = %tenant_id,
                        "reaper: IdP plugin returned UnsupportedOperation but idp.required=true; \
                         parking row out of the retry loop (operator action required to deprovision \
                         vendor-side state before AM row removal)"
                    );
                    return ReaperOutcome::Terminal;
                }
                ReaperOutcome::Compensable("compensated")
            }
            Err(IdpDeprovisionFailure::NotFound { .. }) => {
                // Vendor reports the tenant is already gone (typical
                // 404 / 410 from the SDK). Per the IdP trait
                // contract this is success-equivalent: continue with
                // local DB teardown, but emit a distinct metric
                // label so dashboards can spot the difference
                // between "we deleted it" and "it was already gone"
                // — the latter often signals a lost claim or
                // cross-system inconsistency.
                ReaperOutcome::Compensable("already_absent")
            }
            Err(IdpDeprovisionFailure::Retryable { detail }) => {
                // Vendor SDK detail strings may carry hostnames,
                // endpoint paths, or token-bearing fragments — same
                // class of secrets the `am.idp` mapping in
                // `domain/idp` redacts. The reaper logs into a
                // longer-retention target (`am.retention`), so the
                // raw text MUST be redacted here too. Operators
                // correlate via the FNV-1a digest plus character
                // length, identical to the provision-side redaction
                // contract.
                let (digest, len) = crate::domain::idp::redact_provider_detail(&detail);
                warn!(
                    target: "am.retention",
                    tenant_id = %tenant_id,
                    provider_detail_digest = digest,
                    provider_detail_len = len,
                    "reaper: retryable IdP failure; deferring to next tick (raw detail redacted; correlate via digest)"
                );
                emit_metric(
                    AM_TENANT_RETENTION,
                    MetricKind::Counter,
                    &[
                        ("retention_job", "provisioning_reaper"),
                        ("outcome", "retryable"),
                    ],
                );
                ReaperOutcome::Defer
            }
            Err(IdpDeprovisionFailure::Terminal { detail }) => {
                // Per the SDK contract, `Terminal` means the vendor
                // refused to deprovision and operator intervention is
                // required. Stamp `terminal_failure_at` (in
                // `compensate_terminal_provisioning_row`) so the
                // scan filter excludes the row until an operator
                // clears the column or hard-deletes the row.
                //
                // No `terminal` metric is emitted here: the mark
                // UPDATE in `mark_terminal_provisioning_row` may
                // still fail (`Ok(false)` on lost claim, `Err` on
                // storage fault) and emit a different label. Emitting
                // `terminal` speculatively at classification time
                // would inflate the dashboard counter relative to the
                // number of rows whose `terminal_failure_at` actually
                // landed. The successful-mark counter is emitted from
                // `mark_terminal_provisioning_row` `Ok(true)`.
                let (digest, len) = crate::domain::idp::redact_provider_detail(&detail);
                warn!(
                    target: "am.retention",
                    tenant_id = %tenant_id,
                    provider_detail_digest = digest,
                    provider_detail_len = len,
                    "reaper: terminal IdP failure; marking row terminal_failure_at and parking out of the retry loop (operator action required; raw detail redacted, correlate via digest)"
                );
                ReaperOutcome::Terminal
            }
            // `IdpDeprovisionFailure` is `#[non_exhaustive]`; the
            // wildcard guards against a future SDK variant landing
            // without a service-side classification update.
            Err(_) => {
                warn!(
                    target: "am.retention",
                    tenant_id = %tenant_id,
                    "reaper: unknown IdpDeprovisionFailure variant; deferring as retryable"
                );
                emit_metric(
                    AM_TENANT_RETENTION,
                    MetricKind::Counter,
                    &[
                        ("retention_job", "provisioning_reaper"),
                        ("outcome", "unknown"),
                    ],
                );
                ReaperOutcome::Defer
            }
        }
    }

    /// Physically remove the `Provisioning` row via
    /// `TenantRepo::compensate_provisioning` and emit a structured
    /// `am.events` log line plus an `am_tenant_retention_total` metric
    /// sample for a row whose `IdP`-side cleanup is classified as
    /// success-equivalent. The `actor=system` audit record required
    /// by FEATURE/PRD/DESIGN is **deferred to a follow-up** until
    /// the platform append-only audit sink (event-bus) lands — see
    /// `TODO(events)` below; the log line on the `am.events` target
    /// is the v1 stand-in observers can correlate against the metric
    /// until that sink exists.
    ///
    /// Provisioning rows never become SDK-visible, so deleting
    /// directly here keeps the outcome local to one reaper tick —
    /// routing through the soft-delete + retention pipeline would
    /// leak tombstones (`Deleted` rows visible in the DB long after
    /// `IdP` teardown finished) and force a re-claim/re-process on a
    /// later tick.
    ///
    /// Releases the claim only on infra failure (the row is gone on
    /// success, including the `claimed_by` column).
    async fn compensate_provisioning_row(
        &self,
        scope: &AccessScope,
        tenant_id: uuid::Uuid,
        claimed_by: uuid::Uuid,
        outcome_label: &'static str,
        result: &mut ReaperResult,
    ) {
        // Reaper path: pass `Some(claimed_by)` so the repo fences
        // the DELETE on this worker's claim AND on
        // `terminal_failure_at IS NULL`. Closes the
        // RETENTION_CLAIM_TTL race where a peer worker re-claimed
        // (or terminal-stamped) the row during this worker's IdP
        // round-trip — without the fence, this worker's `Compensable`
        // verdict would silently erase the peer's work.
        if let Err(err) = self
            .repo
            .compensate_provisioning(scope, tenant_id, Some(claimed_by))
            .await
        {
            // A storage fault on the compensation delete is an infra
            // blip (pool exhausted, SERIALIZABLE retry budget gone,
            // etc.) OR a legitimate "no longer Provisioning" Conflict
            // raised by the repo's status-fence. Either way, defer
            // the row, release the claim so a peer can retry, emit a
            // dedicated `compensate_failed` metric so the infra fault
            // stays observable distinct from IdP-side failures.
            warn!(
                target: "am.retention",
                tenant_id = %tenant_id,
                error = %err,
                "reaper: compensate_provisioning failed"
            );
            result.deferred += 1;
            emit_metric(
                AM_TENANT_RETENTION,
                MetricKind::Counter,
                &[
                    ("retention_job", "provisioning_reaper"),
                    ("outcome", "compensate_failed"),
                ],
            );
            self.release_claim(scope, tenant_id, claimed_by).await;
            return;
        }
        // Match on the outcome label so the operator-visible counter
        // reflects whether we actively cleaned the row or merely
        // observed it was already absent on the vendor side. Both
        // increment via the metric label too — the dashboard split
        // and the result-struct split are in lockstep.
        match outcome_label {
            "already_absent" => result.already_absent += 1,
            _ => result.compensated += 1,
        }
        emit_metric(
            AM_TENANT_RETENTION,
            MetricKind::Counter,
            &[
                ("retention_job", "provisioning_reaper"),
                ("outcome", outcome_label),
            ],
        );
        // TODO(events): emit AM event when platform event-bus lands.
        tracing::info!(
            target: "am.events",
            kind = "provisioningReaperCompensated",
            actor = "system",
            tenant_id = %tenant_id,
            outcome = outcome_label,
            "am tenant state changed"
        );
        // compensate_provisioning is a physical delete (claimed_by gone);
        // explicit release on success would warn against a missing row.
        // `scope` and `claimed_by` stay function args — the storage-error
        // branch above still releases the claim so a peer can retry.
    }

    /// Stamp `terminal_failure_at` on the row via
    /// [`TenantRepo::mark_provisioning_terminal_failure`] and bump
    /// `result.terminal`. The marker keeps the row out of the
    /// `scan_stuck_provisioning` retry loop until an operator
    /// clears it; the reaper releases the claim afterwards (whether
    /// the mark landed or not) so the row's columns remain tidy
    /// regardless of whether a peer reaper would have eventually
    /// observed the same Terminal outcome.
    ///
    /// On infra failure of the mark UPDATE itself (storage fault),
    /// the row falls through to `result.deferred` instead — the
    /// scan filter will not exclude it on the next tick, and a peer
    /// (or this worker on a later tick) will retry the
    /// classification + mark sequence.
    async fn mark_terminal_provisioning_row(
        &self,
        scope: &AccessScope,
        tenant_id: uuid::Uuid,
        claimed_by: uuid::Uuid,
        now: OffsetDateTime,
        result: &mut ReaperResult,
    ) {
        match self
            .repo
            .mark_provisioning_terminal_failure(scope, tenant_id, claimed_by, now)
            .await
        {
            Ok(true) => {
                result.terminal += 1;
                // Emit the `terminal` outcome counter only after the
                // mark UPDATE confirmed `Ok(true)`. Counterpart to
                // the comment in `classify_deprovision`'s
                // `IdpDeprovisionFailure::Terminal` arm: speculative
                // emission there would inflate the metric over rows
                // whose `terminal_failure_at` never actually landed
                // (lost claim or storage fault).
                emit_metric(
                    AM_TENANT_RETENTION,
                    MetricKind::Counter,
                    &[
                        ("retention_job", "provisioning_reaper"),
                        ("outcome", "terminal"),
                    ],
                );
            }
            Ok(false) => {
                // Either the row left `Provisioning` between the
                // IdP round-trip and our mark write (treated as
                // success-equivalent for idempotency — the row is
                // no longer the reaper's concern) or this worker
                // lost its claim. Counted as `deferred` because no
                // terminal stamp was actually persisted; the
                // scan-filter still applies on the next tick if
                // some other party already stamped the row, or the
                // row is gone entirely.
                result.deferred += 1;
                emit_metric(
                    AM_TENANT_RETENTION,
                    MetricKind::Counter,
                    &[
                        ("retention_job", "provisioning_reaper"),
                        ("outcome", "terminal_lost_claim"),
                    ],
                );
            }
            Err(err) => {
                warn!(
                    target: "am.retention",
                    tenant_id = %tenant_id,
                    error = %err,
                    "reaper: mark_provisioning_terminal_failure failed; deferring"
                );
                result.deferred += 1;
                emit_metric(
                    AM_TENANT_RETENTION,
                    MetricKind::Counter,
                    &[
                        ("retention_job", "provisioning_reaper"),
                        ("outcome", "terminal_mark_failed"),
                    ],
                );
            }
        }
        self.release_claim(scope, tenant_id, claimed_by).await;
    }

    /// Release the per-row claim, swallowing storage errors so a
    /// transient fault never leaks past the reaper tick. The
    /// `RETENTION_CLAIM_TTL` window in
    /// [`crate::infra::storage::repo_impl::retention`] is the
    /// fallback if this call doesn't land.
    async fn release_claim(
        &self,
        scope: &AccessScope,
        tenant_id: uuid::Uuid,
        claimed_by: uuid::Uuid,
    ) {
        if let Err(err) = self
            .repo
            .clear_retention_claim(scope, tenant_id, claimed_by)
            .await
        {
            warn!(
                target: "am.retention",
                tenant_id = %tenant_id,
                error = %err,
                "reaper: failed to clear claim; will be released by RETENTION_CLAIM_TTL"
            );
            // Mirrors the symmetric signal in `hard_delete_batch`:
            // separates a healthy claim release from a
            // storage-fault-induced stale-claim cliff.
            emit_metric(
                AM_TENANT_RETENTION,
                MetricKind::Counter,
                &[
                    ("retention_job", "provisioning_reaper"),
                    ("outcome", "claim_clear_failed"),
                ],
            );
        }
    }
}
