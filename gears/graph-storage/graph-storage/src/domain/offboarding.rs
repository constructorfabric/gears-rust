//! Tenant offboarding (`cpt-cf-graph-storage-fr-tenant-offboarding`, DESIGN
//! § Tenant Offboarding and Deletion Monotonicity).
//!
//! The protocol's first half -- accept, fence, delete, record -- without the
//! platform deletion ledger, which does not exist yet: the gear asks the
//! platform's tenant resolver whether the tenant is marked deleted instead of
//! checking a deletion generation, and readiness keeps
//! `tenant_reconciliation` as not implemented.
//!
//! The service starts an offboarding; the gear's worker ([`run_pending`])
//! finishes it, so a deletion that began always ends, whether or not anyone
//! keeps asking, and a restart picks it up again from the store.

use std::collections::HashMap;
use std::num::NonZeroU64;
use std::time::Duration;

use async_trait::async_trait;
use graph_storage_sdk::models::TenantOffboarding;
use graph_storage_sdk::plugin_api::{GraphStoreError, GraphStoreV1};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::diagnostics::logged;
use crate::domain::error::DomainError;

/// What the platform says about a tenant, as far as offboarding needs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TenantState {
    /// Marked deleted: offboarding may start.
    Deleted,
    /// Known and not deleted.
    Live,
    /// Not known to the platform: never created, already removed for good,
    /// or not visible to the caller. The platform does not say which, so an
    /// irreversible deletion does not start on it.
    Unknown,
}

/// The platform's answer about a tenant.
#[async_trait]
pub trait TenantDirectory: Send + Sync {
    /// What the platform says about `tenant`, within `within`.
    ///
    /// # Errors
    ///
    /// `Unavailable` when the platform cannot be asked, `Deadline` when it
    /// does not answer in time, `AccessDenied` when it refuses the caller.
    async fn state(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        within: Duration,
    ) -> Result<TenantState, DomainError>;
}

/// No directory at all: every offboarding is refused as a dependency outage.
/// What the services run with until one is wired, so a deployment that
/// cannot ask the platform never fences a tenant on a permission alone.
pub struct NoTenantDirectory;

#[async_trait]
impl TenantDirectory for NoTenantDirectory {
    async fn state(
        &self,
        _ctx: &SecurityContext,
        _tenant: Uuid,
        _within: Duration,
    ) -> Result<TenantState, DomainError> {
        Err(DomainError::Unavailable {
            detail: "no tenant resolver is wired, so no tenant can be confirmed deleted".to_owned(),
        })
    }
}

/// How the worker deletes. Built through [`WorkerSettings::new`], so the
/// worker can neither spin nor take nothing.
#[derive(Clone, Copy, Debug)]
pub struct WorkerSettings {
    batch: NonZeroU64,
    interval: Duration,
    step_timeout: Duration,
}

impl WorkerSettings {
    /// `None` for a zero interval or a zero step timeout.
    #[must_use]
    pub fn new(batch: NonZeroU64, interval: Duration, step_timeout: Duration) -> Option<Self> {
        (!interval.is_zero() && !step_timeout.is_zero()).then_some(Self {
            batch,
            interval,
            step_timeout,
        })
    }

    /// Rows per step.
    #[must_use]
    pub fn batch(&self) -> NonZeroU64 {
        self.batch
    }

    /// The pause between passes, and the unit of the backoff.
    #[must_use]
    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// The longest one step may take before it counts as failed.
    #[must_use]
    pub fn step_timeout(&self) -> Duration {
        self.step_timeout
    }
}

/// Failed steps in a row after which a tenant's failures log at `error`.
const ESCALATE_AFTER: u32 = 5;

/// The longest a failing tenant waits before its next attempt.
const MAX_BACKOFF: Duration = Duration::from_secs(3_600);

/// Steps one tenant gets per pass before the worker moves on, so a large
/// tenant does not hold up the others for a whole deletion.
const MAX_STEPS_PER_PASS: usize = 1_000;

/// How long a tenant whose last `failures` steps failed waits after the last
/// one before it is tried again: the interval, doubled per failure, capped.
fn backoff(interval: Duration, failures: u32) -> Duration {
    if failures == 0 {
        return Duration::ZERO;
    }
    interval
        .saturating_mul(2_u32.saturating_pow(failures.min(16)))
        .min(MAX_BACKOFF)
}

/// Whether a tenant whose last `failures` steps failed, the last at `last`,
/// is due for another attempt at `now`.
fn due(
    failures: u32,
    last: Option<OffsetDateTime>,
    interval: Duration,
    now: OffsetDateTime,
) -> bool {
    let Some(last) = last else {
        return true;
    };
    if failures == 0 {
        return true;
    }
    let wait = time::Duration::try_from(backoff(interval, failures)).unwrap_or(time::Duration::MAX);
    now - last >= wait
}

/// What one replica's worker remembers between passes: the failures it saw
/// itself. The record keeps them too, but in a database outage the note on
/// the record fails with the step, and the backoff and the escalation must
/// not depend on the store being healthy enough not to need them.
#[derive(Debug, Default)]
pub struct WorkerState {
    streaks: HashMap<Uuid, Streak>,
    listing_failures: u32,
}

#[derive(Clone, Copy, Debug)]
struct Streak {
    failures: u32,
    last: OffsetDateTime,
}

impl WorkerState {
    /// The failures to back off and escalate on: the larger of what the
    /// record says and what this worker saw, with the later of their times.
    fn failures_of(&self, record: &TenantOffboarding) -> (u32, Option<OffsetDateTime>) {
        let recorded = (
            record.failures,
            record.last_failure.as_ref().map(|failure| failure.at),
        );
        match self.streaks.get(&record.tenant_id) {
            None => recorded,
            Some(seen) => (
                recorded.0.max(seen.failures),
                Some(recorded.1.map_or(seen.last, |at| at.max(seen.last))),
            ),
        }
    }
}

/// One pass: work every fenced tenant's deletion that is due, starting at a
/// random one so replicas started together do not all queue on the same
/// tenant.
///
/// A failure on one tenant is noted and logged, and the pass moves on; the
/// tenant waits out its backoff. A tenant gets at most a fixed number of
/// steps per pass. Stops when `cancel` fires, a running step included.
/// Answers how many deletions it finished.
pub async fn run_pending(
    store: &dyn GraphStoreV1,
    settings: WorkerSettings,
    state: &mut WorkerState,
    cancel: &CancellationToken,
) -> usize {
    let Some(mut pending) = list_pending(store, state).await else {
        return 0;
    };
    if !pending.is_empty() {
        use rand::RngExt as _;
        let start = rand::rng().random_range(0..pending.len());
        pending.rotate_left(start);
    }
    state
        .streaks
        .retain(|tenant, _| pending.iter().any(|record| record.tenant_id == *tenant));
    let now = OffsetDateTime::now_utc();
    let mut pass = Pass::default();
    for record in &pending {
        let (failures, last) = state.failures_of(record);
        if !due(failures, last, settings.interval, now) {
            pass.waiting += 1;
            continue;
        }
        match finish(store, record, settings, state, cancel).await {
            Finish::Done => pass.finished += 1,
            Finish::Failed => pass.failed += 1,
            Finish::Yielded => pass.yielded += 1,
            Finish::Cancelled => break,
        }
    }
    pass.log(pending.len());
    pass.finished
}

/// The pending deletions, or `None` when they cannot be listed -- logged,
/// and at `error` once it keeps happening, since then no deletion runs.
async fn list_pending(
    store: &dyn GraphStoreV1,
    state: &mut WorkerState,
) -> Option<Vec<TenantOffboarding>> {
    match store.pending_offboardings().await {
        Ok(pending) => {
            state.listing_failures = 0;
            Some(pending)
        }
        Err(error) => {
            state.listing_failures = state.listing_failures.saturating_add(1);
            if state.listing_failures >= ESCALATE_AFTER {
                tracing::error!(
                    failures = state.listing_failures,
                    error = %logged(&error),
                    "offboarding: the pending deletions cannot be listed; no deletion runs"
                );
            } else {
                tracing::warn!(error = %logged(&error), "offboarding: the pending deletions cannot be listed");
            }
            None
        }
    }
}

/// What one pass did, for the per-pass log line.
#[derive(Default)]
struct Pass {
    finished: usize,
    failed: usize,
    waiting: usize,
    yielded: usize,
}

impl Pass {
    /// At `info` while anything is pending, so a stuck queue is visible at
    /// the default level; nothing when there is nothing to do.
    fn log(&self, pending: usize) {
        if pending > 0 {
            tracing::info!(
                pending,
                finished = self.finished,
                failed = self.failed,
                waiting = self.waiting,
                unfinished = self.yielded,
                "offboarding: a worker pass ran"
            );
        }
    }
}

enum Finish {
    Done,
    Failed,
    /// Out of steps for this pass; the next pass goes on.
    Yielded,
    Cancelled,
}

/// Step one tenant's deletion until it completes, fails, uses up its steps
/// for the pass, or is cancelled. Each step is bounded by the step timeout
/// and raced against `cancel`; a step dropped either way rolls its
/// transaction back.
async fn finish(
    store: &dyn GraphStoreV1,
    record: &TenantOffboarding,
    settings: WorkerSettings,
    state: &mut WorkerState,
    cancel: &CancellationToken,
) -> Finish {
    let tenant = record.tenant_id;
    for _ in 0..MAX_STEPS_PER_PASS {
        let step = tokio::select! {
            biased;
            () = cancel.cancelled() => return Finish::Cancelled,
            step = tokio::time::timeout(
                settings.step_timeout,
                store.offboard_step(tenant, settings.batch),
            ) => step,
        };
        match step {
            Ok(Ok(done)) => {
                // Progress: whatever failed before is behind it.
                state.streaks.remove(&tenant);
                if done.is_complete() {
                    log_completed(&done);
                    return Finish::Done;
                }
            }
            Ok(Err(error)) => {
                note_failure(store, record, state, &error).await;
                return Finish::Failed;
            }
            Err(_) => {
                note_failure(store, record, state, &GraphStoreError::Deadline).await;
                return Finish::Failed;
            }
        }
    }
    Finish::Yielded
}

/// Put a failed step on the record, in this worker's memory and in the log,
/// louder once it keeps happening.
async fn note_failure(
    store: &dyn GraphStoreV1,
    record: &TenantOffboarding,
    state: &mut WorkerState,
    error: &GraphStoreError,
) {
    let tenant = record.tenant_id;
    let (before, _) = state.failures_of(record);
    let seen = before.saturating_add(1);
    state.streaks.insert(
        tenant,
        Streak {
            failures: seen,
            last: OffsetDateTime::now_utc(),
        },
    );
    let failures = match store.record_offboarding_failure(tenant, error).await {
        Ok(noted) => noted.failures.max(seen),
        Err(noted) => {
            tracing::warn!(
                tenant = %tenant,
                error = %logged(&noted),
                "offboarding: the failure could not be noted on the record"
            );
            seen
        }
    };
    log_failure(tenant, failures, error);
}

fn log_failure(tenant: Uuid, failures: u32, error: &GraphStoreError) {
    if failures >= ESCALATE_AFTER {
        tracing::error!(
            tenant = %tenant,
            failures,
            error = %logged(error),
            "offboarding: the deletion keeps failing; it is retried with backoff"
        );
    } else {
        tracing::warn!(
            tenant = %tenant,
            failures,
            error = %logged(error),
            "offboarding: a deletion step failed; it is retried with backoff"
        );
    }
}

fn log_completed(record: &TenantOffboarding) {
    let removed = &record.removed;
    tracing::info!(
        tenant = %record.tenant_id,
        removed = removed.total(),
        edges = removed.edges,
        nodes = removed.nodes,
        types = removed.types,
        scopes = removed.scopes,
        receipts = removed.receipts,
        namespaces = removed.namespaces,
        meta = removed.meta,
        "offboarding: the tenant's graph is removed"
    );
}

/// The worker: a pass, then a pause of the interval plus up to a tenth of
/// it more, so replicas started together drift apart, until `cancel` fires.
//
// cancel-safe: dropped at shutdown, a step's transaction rolls back with its
// connection, the record keeps the counts of the steps that committed, and
// the next pass -- here or on another replica -- resumes from the store.
pub async fn run_worker(
    store: std::sync::Arc<dyn GraphStoreV1>,
    settings: WorkerSettings,
    cancel: CancellationToken,
) {
    use rand::RngExt as _;
    let mut state = WorkerState::default();
    loop {
        run_pending(store.as_ref(), settings, &mut state, &cancel).await;
        let spread = u64::try_from((settings.interval / 10).as_millis()).unwrap_or(0);
        let jitter = Duration::from_millis(rand::rng().random_range(0..=spread));
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(settings.interval + jitter) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failing_tenant_waits_longer_after_each_failure_up_to_an_hour() {
        let interval = Duration::from_secs(10);
        assert_eq!(backoff(interval, 0), Duration::ZERO);
        assert_eq!(backoff(interval, 1), Duration::from_secs(20));
        assert_eq!(backoff(interval, 2), Duration::from_secs(40));
        assert_eq!(backoff(interval, 30), MAX_BACKOFF);
    }

    /// A failed tenant is skipped until its backoff runs out, and due again
    /// from that moment; with no failure, or none recorded, it is due now.
    #[test]
    fn a_failed_tenant_is_due_again_once_its_backoff_runs_out() {
        let interval = Duration::from_secs(10);
        let last = OffsetDateTime::now_utc();
        let wait = time::Duration::seconds(20);
        assert!(!due(1, Some(last), interval, last));
        assert!(!due(
            1,
            Some(last),
            interval,
            last + wait - time::Duration::milliseconds(1)
        ));
        assert!(due(1, Some(last), interval, last + wait));
        assert!(due(1, Some(last), interval, last + wait + wait));
        assert!(due(0, Some(last), interval, last));
        assert!(due(3, None, interval, last));
    }

    #[test]
    fn the_settings_refuse_an_interval_or_a_step_timeout_of_zero() {
        let batch = NonZeroU64::MIN;
        let second = Duration::from_secs(1);
        assert!(WorkerSettings::new(batch, Duration::ZERO, second).is_none());
        assert!(WorkerSettings::new(batch, second, Duration::ZERO).is_none());
        assert!(WorkerSettings::new(batch, second, second).is_some());
    }
}
