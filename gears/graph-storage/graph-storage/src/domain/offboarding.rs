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

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU64;
use std::time::Duration;

use async_trait::async_trait;
use graph_storage_sdk::models::TenantOffboarding;
use graph_storage_sdk::plugin_api::{GraphStoreError, GraphStoreV1};
use time::OffsetDateTime;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use toolkit_macros::domain_model;

use crate::domain::diagnostics::logged;
use crate::domain::error::DomainError;

/// What the platform says about a tenant, as far as offboarding needs it.
#[domain_model]
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
#[domain_model]
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
#[domain_model]
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
#[domain_model]
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
    run_pending_at(store, settings, state, cancel, OffsetDateTime::now_utc()).await
}

/// [`run_pending`] as if it were `now`: which tenants are due is decided
/// against it. The seam a test uses to run a pass after a backoff.
pub async fn run_pending_at(
    store: &dyn GraphStoreV1,
    settings: WorkerSettings,
    state: &mut WorkerState,
    cancel: &CancellationToken,
    now: OffsetDateTime,
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
    // The record as the last step left it: a step that made progress cleared
    // the failures the pass started with, and a failure after it counts from
    // there, as the store does.
    let mut latest = record.clone();
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
                latest = done;
            }
            Ok(Err(error)) => {
                note_failure(store, &latest, settings, state, cancel, &error).await;
                return Finish::Failed;
            }
            Err(_) => {
                note_failure(
                    store,
                    &latest,
                    settings,
                    state,
                    cancel,
                    &GraphStoreError::Deadline,
                )
                .await;
                return Finish::Failed;
            }
        }
    }
    Finish::Yielded
}

/// Put a failed step on the record, in this worker's memory and in the log,
/// louder once it keeps happening.
///
/// The note on the record is bounded by the step timeout and raced against
/// `cancel`: a step that timed out may still hold the record's row, and the
/// note must not hold up the worker behind it. A note that does not land
/// leaves the count in this worker's memory.
async fn note_failure(
    store: &dyn GraphStoreV1,
    record: &TenantOffboarding,
    settings: WorkerSettings,
    state: &mut WorkerState,
    cancel: &CancellationToken,
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
    let noted = tokio::select! {
        biased;
        () = cancel.cancelled() => Err(GraphStoreError::Unavailable {
            reason: "the worker is stopping".to_owned(),
        }),
        noted = tokio::time::timeout(
            settings.step_timeout,
            store.record_offboarding_failure(tenant, error),
        ) => noted.unwrap_or(Err(GraphStoreError::Deadline)),
    };
    let failures = match noted {
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

/// The gear's offboarding background, until `cancel` fires: the worker, and
/// the fenced tenants read again every refresh. A store that does not
/// offboard needs neither, and is asked nothing.
pub async fn run_background(
    store: std::sync::Arc<dyn GraphStoreV1>,
    fenced: &FencedTenants,
    settings: WorkerSettings,
    cancel: CancellationToken,
) {
    if !store.capabilities().offboarding {
        tracing::info!("offboarding: the store does not offboard; no worker runs");
        return;
    }
    log_started(settings);
    tokio::join!(
        run_worker(std::sync::Arc::clone(&store), settings, cancel.clone()),
        fenced.keep_fresh(store.as_ref(), &cancel),
    );
    tracing::info!("offboarding: the worker stopped");
}

fn log_started(settings: WorkerSettings) {
    tracing::info!(
        batch = settings.batch().get(),
        interval_secs = settings.interval().as_secs(),
        step_timeout_secs = settings.step_timeout().as_secs(),
        "offboarding: the worker started"
    );
}

// --- the fenced tenants, in memory ------------------------------------------

/// How often the gear's background task reads the fenced tenants again. A
/// fence this replica did not make is honoured on the read path about this
/// long after it committed; writes check the fence at commit whatever the
/// set holds.
pub const FENCE_REFRESH: Duration = Duration::from_secs(10);

/// Refreshes the set may miss before a request stops trusting it.
const TRUSTED_FOR_REFRESHES: u32 = 3;

/// The longest one read of the fenced tenants may take.
const FENCE_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// The tenants ever fenced, held in memory for the request path.
///
/// A fence is permanent and only ever added, so the set never holds a tenant
/// it should not: it can only miss one fenced since it was read. The gear's
/// background task reads it again every refresh ([`FencedTenants::keep_fresh`])
/// and the request path reads memory only. A fence made here is in the set
/// at once. A set not read for three refreshes -- the store keeps failing, or
/// no background read has run yet -- is not trusted: the request reads the
/// store itself, bounded, and is refused when that fails.
///
/// A write's own transaction checks the fence at commit, so a miss here
/// never lets a write through.
#[domain_model]
#[derive(Debug)]
pub struct FencedTenants {
    known: std::sync::RwLock<Known>,
    /// One read of the store at a time; callers that find the set stale wait
    /// for the one reading it rather than each reading it again.
    reading: tokio::sync::Mutex<Reading>,
    /// Reads of the store finished so far, so a caller can tell a read that
    /// finished while it waited.
    reads: std::sync::atomic::AtomicU64,
    refresh: Duration,
}

#[derive(Debug, Default)]
struct Known {
    tenants: std::sync::Arc<HashSet<Uuid>>,
    /// The last successful read; none before the first.
    read_at: Option<Instant>,
}

#[derive(Debug, Default)]
struct Reading {
    /// Failed reads in a row.
    failures: u32,
    /// The read that last failed, by its number, so callers that waited for
    /// it fail with it rather than queue a read each on a store that does
    /// not answer.
    failed: Option<u64>,
}

impl Default for FencedTenants {
    fn default() -> Self {
        Self::refreshed_every(FENCE_REFRESH)
    }
}

impl FencedTenants {
    #[must_use]
    pub fn refreshed_every(refresh: Duration) -> Self {
        Self {
            known: std::sync::RwLock::new(Known::default()),
            reading: tokio::sync::Mutex::new(Reading::default()),
            reads: std::sync::atomic::AtomicU64::new(0),
            refresh,
        }
    }

    /// How long after its last successful read the set serves a request.
    fn trusted_for(&self) -> Duration {
        self.refresh.saturating_mul(TRUSTED_FOR_REFRESHES)
    }

    /// The fenced tenants, for a request: the set in memory while it was read
    /// within three refreshes; otherwise read from `store` now.
    ///
    /// Cancel-safe: the set is replaced only after the read returned, in one
    /// step, so a call dropped while it waits or reads leaves it as it was.
    ///
    /// # Errors
    ///
    /// The store's, or `Deadline`, when the set is not trusted and cannot be
    /// read; `Unavailable` when a read this call waited for failed.
    pub async fn current(
        &self,
        store: &dyn GraphStoreV1,
    ) -> Result<std::sync::Arc<HashSet<Uuid>>, GraphStoreError> {
        let trusted = self.trusted_for();
        if let Some(set) = self.read_within(trusted) {
            return Ok(set);
        }
        self.read(store, trusted).await
    }

    /// Read the set again every refresh until `cancel` fires, so requests
    /// read memory only. A failed read is logged, at `error` once it keeps
    /// failing, and leaves the set as it was.
    //
    // cancel-safe: as `current`.
    pub async fn keep_fresh(&self, store: &dyn GraphStoreV1, cancel: &CancellationToken) {
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => return,
                _ = self.read(store, self.refresh) => {}
            }
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(self.refresh.max(Duration::from_secs(1))) => {}
            }
        }
    }

    /// Read the set from `store` unless it was read within `fresh_for`,
    /// one caller at a time.
    async fn read(
        &self,
        store: &dyn GraphStoreV1,
        fresh_for: Duration,
    ) -> Result<std::sync::Arc<HashSet<Uuid>>, GraphStoreError> {
        let asked = self.reads.load(std::sync::atomic::Ordering::SeqCst);
        let mut reading = self.reading.lock().await;
        // Another caller may have read it while this one waited.
        if let Some(set) = self.read_within(fresh_for) {
            return Ok(set);
        }
        if reading.failed.is_some_and(|read| read > asked) {
            return Err(GraphStoreError::Unavailable {
                reason: "the fenced tenants cannot be read".to_owned(),
            });
        }
        let read = tokio::time::timeout(FENCE_READ_TIMEOUT, store.all_fenced_tenant_ids())
            .await
            .unwrap_or(Err(GraphStoreError::Deadline));
        let number = self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        match read {
            Ok(ids) => {
                *reading = Reading::default();
                Ok(self.replace(ids))
            }
            Err(error) => {
                reading.failures = reading.failures.saturating_add(1);
                reading.failed = Some(number);
                if reading.failures >= ESCALATE_AFTER {
                    tracing::error!(
                        failures = reading.failures,
                        error = %logged(&error),
                        "offboarding: the fenced tenants keep failing to read; requests are \
                         refused once the last read is too old"
                    );
                } else {
                    tracing::warn!(
                        failures = reading.failures,
                        error = %logged(&error),
                        "offboarding: the fenced tenants could not be read"
                    );
                }
                Err(error)
            }
        }
    }

    /// Take `ids` as the set, keeping the fences this replica made while the
    /// store was read.
    fn replace(&self, ids: Vec<Uuid>) -> std::sync::Arc<HashSet<Uuid>> {
        let mut known = self
            .known
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut tenants: HashSet<Uuid> = ids.into_iter().collect();
        tenants.extend(known.tenants.iter().copied());
        known.tenants = std::sync::Arc::new(tenants);
        known.read_at = Some(Instant::now());
        std::sync::Arc::clone(&known.tenants)
    }

    /// The set, when it was read within `within`.
    fn read_within(&self, within: Duration) -> Option<std::sync::Arc<HashSet<Uuid>>> {
        let known = self
            .known
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        known
            .read_at
            .is_some_and(|at| at.elapsed() < within)
            .then(|| std::sync::Arc::clone(&known.tenants))
    }

    /// Add a tenant this replica fenced, so its requests see the fence at
    /// once rather than at the next read.
    pub(crate) fn add(&self, tenant: Uuid) {
        let mut known = self
            .known
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !known.tenants.contains(&tenant) {
            let mut tenants = (*known.tenants).clone();
            tenants.insert(tenant);
            known.tenants = std::sync::Arc::new(tenants);
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

    // --- the fenced tenants ------------------------------------------------

    use std::sync::Arc;

    use crate::infra::fake_store::FakeGraphStore;

    async fn fence(store: &FakeGraphStore, tenant: Uuid) {
        let scope = toolkit_security::AccessScope::for_tenant(tenant);
        let ctx = graph_storage_sdk::plugin_api::StoreCtx {
            tenant,
            scope: &scope,
            subject: graph_storage_sdk::models::Subject {
                subject_id: Uuid::now_v7(),
                subject_type: None,
            },
            snapshot: None,
            budget: graph_storage_sdk::models::RemainingBudget::starting_now(Duration::from_secs(
                30,
            )),
            cancel: CancellationToken::new(),
        };
        store.fence_tenant(&ctx, None).await.expect("the fence");
    }

    /// The set is read every ten seconds, and a request trusts it for three
    /// refreshes: within them it is served from memory, past them the
    /// request reads it again.
    #[tokio::test(start_paused = true)]
    async fn a_request_reads_memory_for_three_refreshes_then_the_store() {
        assert_eq!(
            FENCE_REFRESH,
            Duration::from_secs(10),
            "DESIGN promises 10 s"
        );
        let store = FakeGraphStore::new();
        let fenced = FencedTenants::default();
        let tenant = Uuid::now_v7();

        assert!(fenced.current(&store).await.expect("read").is_empty());
        assert_eq!(store.fence_reads(), 1);
        fence(&store, tenant).await;

        tokio::time::advance(Duration::from_secs(29)).await;
        assert!(
            !fenced
                .current(&store)
                .await
                .expect("memory")
                .contains(&tenant)
        );
        assert_eq!(store.fence_reads(), 1, "served from memory");

        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(
            fenced
                .current(&store)
                .await
                .expect("read")
                .contains(&tenant)
        );
        assert_eq!(store.fence_reads(), 2);
    }

    /// The background task reads the set every refresh, so requests find
    /// another replica's fence within one refresh without reading the store.
    #[tokio::test(start_paused = true)]
    async fn the_background_reads_the_set_every_refresh_off_the_request_path() {
        let store = Arc::new(FakeGraphStore::new());
        let fenced = Arc::new(FencedTenants::default());
        let cancel = CancellationToken::new();
        let task = tokio::spawn({
            let (store, fenced, cancel) = (Arc::clone(&store), Arc::clone(&fenced), cancel.clone());
            async move { fenced.keep_fresh(store.as_ref(), &cancel).await }
        });
        tokio::time::advance(Duration::from_millis(1)).await;
        assert_eq!(store.fence_reads(), 1);

        let tenant = Uuid::now_v7();
        fence(&store, tenant).await;
        tokio::time::advance(FENCE_REFRESH + Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(store.fence_reads(), 2);
        assert!(
            fenced
                .current(store.as_ref())
                .await
                .expect("memory")
                .contains(&tenant)
        );
        assert_eq!(store.fence_reads(), 2, "the request read memory");

        cancel.cancel();
        task.await.expect("the background stops");
    }

    /// A store that does not answer: the first read fails the request; a
    /// failed re-read leaves the set serving while it is trusted, and once
    /// it is too old the request is refused rather than served from it.
    #[tokio::test(start_paused = true)]
    async fn a_set_that_cannot_be_read_fails_closed_once_it_is_too_old() {
        let store = FakeGraphStore::new();
        let fenced = FencedTenants::default();
        store.fail_fence_reads(true);
        assert!(
            fenced.current(&store).await.is_err(),
            "nothing to fall back on"
        );

        store.fail_fence_reads(false);
        fenced
            .current(&store)
            .await
            .expect("the store answers again");
        store.fail_fence_reads(true);
        tokio::time::advance(FENCE_REFRESH).await;
        assert!(
            fenced.read(&store, fenced.refresh).await.is_err(),
            "the background read fails"
        );
        fenced.current(&store).await.expect("still trusted");

        tokio::time::advance(FENCE_REFRESH * 3).await;
        assert!(fenced.current(&store).await.is_err(), "too old to trust");
        store.fail_fence_reads(false);
        fenced
            .current(&store)
            .await
            .expect("served once the store answers");
    }

    /// Callers that find the set stale share one read; when it fails, they
    /// fail with it rather than each read a store that does not answer. A
    /// fence this replica made during the read is kept.
    #[tokio::test(start_paused = true)]
    async fn callers_share_one_read_and_keep_a_fence_made_during_it() {
        let store = Arc::new(FakeGraphStore::new());
        let fenced = Arc::new(FencedTenants::default());
        store.delay_fence_reads(Duration::from_secs(1));
        let reading = |store: &Arc<FakeGraphStore>, fenced: &Arc<FencedTenants>| {
            let (store, fenced) = (Arc::clone(store), Arc::clone(fenced));
            tokio::spawn(async move { fenced.current(store.as_ref()).await })
        };

        let (first, second) = (reading(&store, &fenced), reading(&store, &fenced));
        tokio::time::advance(Duration::from_millis(10)).await;
        let mine = Uuid::now_v7();
        fenced.add(mine);
        for read in [first, second] {
            assert!(read.await.expect("joins").expect("read").contains(&mine));
        }
        assert_eq!(store.fence_reads(), 1, "one read for both");

        tokio::time::advance(FENCE_REFRESH * 3).await;
        store.fail_fence_reads(true);
        let (first, second) = (reading(&store, &fenced), reading(&store, &fenced));
        for read in [first, second] {
            assert!(read.await.expect("joins").is_err());
        }
        assert_eq!(store.fence_reads(), 2, "the waiter failed with the read");
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
