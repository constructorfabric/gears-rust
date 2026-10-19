//! The `LeaseProvider` adapter over the Cluster `LeaderElectionV1` facade (ADR-0008, R-27; feature
//! 01 §3.4).
//!
//! Rating declares one `ClusterProfile` and resolves the facade through it, scoped `bss-rating`.
//! The gear depends on `cluster-sdk` only and declares no `deps = [cluster]` edge, as
//! quota-enforcement: the deployed profile binds the backend.
//!
//! The adapter drives the election watch itself and keeps it, so graceful shutdown can resign
//! and a successor is elected without a TTL wait. On `Lagged` or `Reset` — a gap in the observed
//! transitions — the work stops and resumes only if the backend's lossless status snapshot still
//! reads `Leader`; a worker that outlived a missed `Lost` would be a second active worker. Even
//! then correctness does not depend on the election: every write of the work is a conditional
//! database write that a stale leader's attempt fails.

use std::future::Future;
use std::time::Duration;

use async_trait::async_trait;
use cluster_sdk::{
    ClusterError, ClusterProfile, ElectionConfig, LeaderElectionCapability, LeaderElectionV1,
    LeaderStatus, LeaderWatch, LeaderWatchEvent,
};
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;
use toolkit::client_hub::ClientHub;

use crate::domain::error::{Cause, DomainError};
use crate::domain::leadership::{LEADERSHIP_SCOPE, LeaderWork, LeadershipName, LeaseProvider};

const LOG_TARGET: &str = "bss_rating.leadership";

/// Rating's cluster profile. The profile name appears here and in the operator's cluster
/// configuration, and nowhere else.
#[derive(Debug, Clone, Copy, Default)]
pub struct RatingClusterProfile;

impl ClusterProfile for RatingClusterProfile {
    const NAME: &str = "bss-rating";
}

cluster_sdk::register_cluster_profile!(RatingClusterProfile);

/// The longest a resign on shutdown waits; past it the claim lapses through its TTL anyway.
pub const MAX_RESIGN_WAIT: Duration = Duration::from_secs(5);

/// Election timing plus the stop budget of a leader's work.
#[derive(Debug, Clone, Copy)]
pub struct ElectionTiming {
    config: ElectionConfig,
    stop_timeout: Duration,
}

impl ElectionTiming {
    /// The Cluster SDK's validated election `config`, and the budget a leader's work has to stop
    /// after a loss before it is aborted.
    #[must_use]
    pub const fn new(config: ElectionConfig, stop_timeout: Duration) -> Self {
        Self {
            config,
            stop_timeout,
        }
    }

    /// The budget the work has to stop after a loss before it is aborted.
    #[must_use]
    pub const fn stop_timeout(&self) -> Duration {
        self.stop_timeout
    }

    /// How long joining the election may take: one TTL. A backend that has not answered by then
    /// is treated as unavailable.
    #[must_use]
    pub fn join_timeout(&self) -> Duration {
        self.config.ttl()
    }

    /// How long a resign on shutdown may take: [`MAX_RESIGN_WAIT`], or the TTL when that is
    /// shorter, since the claim lapses after one TTL anyway.
    #[must_use]
    pub fn resign_timeout(&self) -> Duration {
        self.config.ttl().min(MAX_RESIGN_WAIT)
    }
}

/// Resolves `LeaderElectionV1` for Rating's profile, requiring a linearizable backend: an
/// eventually consistent one could elect two leaders on every failover.
///
/// # Errors
///
/// [`DomainError::Coordination`] when the profile binds no backend (R-27) or the requirement
/// fails.
pub async fn resolve_election(hub: &ClientHub) -> Result<LeaderElectionV1, DomainError> {
    let election = LeaderElectionV1::resolver(hub)
        .profile(RatingClusterProfile)
        .require(LeaderElectionCapability::Linearizable)
        .resolve()
        .await
        .map_err(|e| coordination("resolve the leader election", e))?;
    election
        .scoped(LEADERSHIP_SCOPE)
        .map_err(|e| coordination("scope the leader election", e))
}

/// The Cluster-backed leadership port.
#[derive(Clone)]
pub struct ClusterLeaseProvider {
    election: LeaderElectionV1,
    timing: ElectionTiming,
}

impl ClusterLeaseProvider {
    /// A provider over a resolved, `bss-rating`-scoped election facade.
    #[must_use]
    pub fn new(election: LeaderElectionV1, timing: ElectionTiming) -> Self {
        Self { election, timing }
    }
}

#[async_trait]
impl LeaseProvider for ClusterLeaseProvider {
    async fn run_while_leader(
        &self,
        name: LeadershipName,
        shutdown: CancellationToken,
        work: LeaderWork,
    ) -> Result<(), DomainError> {
        // @cpt-begin:cpt-cf-bss-rating-algo-foundation-lease:p1:inst-ls-acquire
        let leaf = name.leaf();
        let join = self.election.elect_with_config(&leaf, self.timing.config);
        let Some(watch) = join_bounded(name, join, &shutdown, self.timing.join_timeout()).await?
        else {
            return Ok(());
        };
        // @cpt-end:cpt-cf-bss-rating-algo-foundation-lease:p1:inst-ls-acquire
        drive(name, watch, shutdown, work, self.timing).await
    }
}

/// Joins an election within `timeout`; `None` when `shutdown` fires first. A backend that accepts
/// the call and never answers would otherwise hold both the leadership loop and the shutdown.
pub(crate) async fn join_bounded(
    name: LeadershipName,
    join: impl Future<Output = Result<LeaderWatch, ClusterError>>,
    shutdown: &CancellationToken,
    timeout: Duration,
) -> Result<Option<LeaderWatch>, DomainError> {
    tokio::select! {
        biased;
        () = shutdown.cancelled() => Ok(None),
        joined = tokio::time::timeout(timeout, join) => match joined {
            Ok(Ok(watch)) => Ok(Some(watch)),
            Ok(Err(err)) => Err(coordination("join the election", err)),
            Err(_elapsed) => Err(DomainError::Coordination(
                format!("joining the {name} election took longer than {timeout:?}"),
                None,
            )),
        },
    }
}

/// What woke the driver.
enum Step {
    Shutdown,
    WorkEnded(Result<(), JoinError>),
    Event(LeaderWatchEvent),
}

/// Drives one election watch until `shutdown` fires, the watch closes or the work ends on its
/// own: the work starts on `Leader` with a child token, is cancelled on `Lost` or `Follower`,
/// aborted after the stop budget if it does not return, and restarted on re-election. On
/// shutdown the running work stops first and the election is resigned afterwards.
///
/// NOT cancel-safe. `serve` aborts the leader task, and so this future, once its join budget
/// runs out. Dropped while it waits in `stop_if_active`, the running work is aborted through
/// [`ActiveWork`]'s `Drop`, mid-transaction, and that transaction rolls back; dropped before
/// `resign_bounded`, the claim is not released and lapses only through its TTL, so a successor
/// waits up to one TTL. Neither breaks correctness, which rests on conditional writes
/// (ADR-0008), but a graceful stop needs the future driven to its end.
pub(crate) async fn drive(
    name: LeadershipName,
    mut watch: LeaderWatch,
    shutdown: CancellationToken,
    work: LeaderWork,
    timing: ElectionTiming,
) -> Result<(), DomainError> {
    // @cpt-begin:cpt-cf-bss-rating-algo-foundation-lease:p1:inst-ls-renew
    let mut active: Option<ActiveWork> = None;
    loop {
        let step = tokio::select! {
            biased;
            () = shutdown.cancelled() => Step::Shutdown,
            ended = work_ended(&mut active) => Step::WorkEnded(ended),
            event = watch.changed() => Step::Event(event),
        };
        match step {
            Step::Shutdown => {
                stop_if_active(name, active.take(), timing.stop_timeout()).await;
                resign_bounded(name, watch, timing).await;
                return Ok(());
            }
            Step::WorkEnded(ended) => {
                // The task is spent: drop it unpolled.
                drop(active.take());
                return work_ended_while_leading(name, ended, watch, timing).await;
            }
            Step::Event(event) => {
                if let Some(outcome) = apply_event(
                    name,
                    event,
                    &watch,
                    &mut active,
                    &work,
                    timing.stop_timeout(),
                )
                .await
                {
                    return outcome;
                }
            }
        }
    }
    // @cpt-end:cpt-cf-bss-rating-algo-foundation-lease:p1:inst-ls-renew
}

/// Resolves when the running work's task ends; never while no work runs.
async fn work_ended(active: &mut Option<ActiveWork>) -> Result<(), JoinError> {
    match active {
        Some(work) => (&mut work.handle).await,
        None => std::future::pending().await,
    }
}

/// The work ended although its term was not cancelled: it returned early or panicked. Holding the
/// claim with no work running would stop the job on every instance, so the election is resigned
/// and the caller starts over after its backoff.
async fn work_ended_while_leading(
    name: LeadershipName,
    ended: Result<(), JoinError>,
    watch: LeaderWatch,
    timing: ElectionTiming,
) -> Result<(), DomainError> {
    let outcome = match &ended {
        Ok(()) => "returned before its term ended",
        Err(err) if err.is_panic() => "panicked",
        Err(_) => "was cancelled",
    };
    // A panic's `JoinError` carries the panic message: it is logged here and kept as the source.
    let join_error = ended.err();
    tracing::error!(
        target: LOG_TARGET,
        %name,
        outcome,
        error = join_error.as_ref().map(tracing::field::display),
        "the leader work ended while this instance led; resigning"
    );
    resign_bounded(name, watch, timing).await;
    Err(DomainError::LeaderWorkEnded(
        format!("the {name} work {outcome}"),
        join_error.map(Cause::new),
    ))
}

/// Resigns while the watch is still owned, so a successor is elected without a TTL wait; a
/// backend slower than the resign budget is left to the TTL.
async fn resign_bounded(name: LeadershipName, watch: LeaderWatch, timing: ElectionTiming) {
    let budget = timing.resign_timeout();
    match tokio::time::timeout(budget, watch.resign()).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            tracing::warn!(target: LOG_TARGET, %name, error = %err, "resign not confirmed; the claim lapses via TTL");
        }
        Err(_elapsed) => {
            tracing::warn!(target: LOG_TARGET, %name, ?budget, "resign took longer than its budget; the claim lapses via TTL");
        }
    }
}

/// Applies one watch event; `Some` when the loop must end.
async fn apply_event(
    name: LeadershipName,
    event: LeaderWatchEvent,
    watch: &LeaderWatch,
    active: &mut Option<ActiveWork>,
    work: &LeaderWork,
    stop_timeout: Duration,
) -> Option<Result<(), DomainError>> {
    match event {
        LeaderWatchEvent::Status(LeaderStatus::Leader) => start_if_idle(name, active, work),
        LeaderWatchEvent::Status(LeaderStatus::Lost | LeaderStatus::Follower) => {
            stop_on_loss(name, active, stop_timeout).await;
        }
        LeaderWatchEvent::Closed(err) => {
            stop_if_active(name, active.take(), stop_timeout).await;
            tracing::warn!(target: LOG_TARGET, %name, error = %err, "the election closed");
            return Some(Err(coordination("the election closed", err)));
        }
        // `Lagged`, `Reset` and any variant this build does not know: a gap in the observed
        // transitions.
        gap => reconcile_from_snapshot(name, &gap, watch, active, work, stop_timeout).await,
    }
    None
}

async fn stop_on_loss(
    name: LeadershipName,
    active: &mut Option<ActiveWork>,
    stop_timeout: Duration,
) {
    if active.is_some() {
        tracing::info!(target: LOG_TARGET, %name, "leadership lost; the work stops after its current transaction");
    }
    stop_if_active(name, active.take(), stop_timeout).await;
}

/// A gap in the event stream may hide a `Lost`: stop, then resume only if the lossless status
/// snapshot still reads `Leader`.
async fn reconcile_from_snapshot(
    name: LeadershipName,
    gap: &LeaderWatchEvent,
    watch: &LeaderWatch,
    active: &mut Option<ActiveWork>,
    work: &LeaderWork,
    stop_timeout: Duration,
) {
    tracing::warn!(target: LOG_TARGET, %name, event = ?gap, "leadership uncertain; the work stops until the status snapshot is read");
    stop_if_active(name, active.take(), stop_timeout).await;
    if watch.status() == LeaderStatus::Leader {
        start_if_idle(name, active, work);
    }
}

fn start_if_idle(name: LeadershipName, active: &mut Option<ActiveWork>, work: &LeaderWork) {
    if active.as_ref().is_none_or(ActiveWork::is_finished) {
        let child = CancellationToken::new();
        let handle = tokio::spawn(work(child.clone()));
        *active = Some(ActiveWork { child, handle });
        tracing::info!(target: LOG_TARGET, %name, "elected; the work runs");
    }
}

async fn stop_if_active(name: LeadershipName, active: Option<ActiveWork>, stop_timeout: Duration) {
    if let Some(active) = active {
        active.stop(name, stop_timeout).await;
    }
}

/// A running leader's work and the token that cancels it.
struct ActiveWork {
    child: CancellationToken,
    handle: JoinHandle<()>,
}

impl ActiveWork {
    fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    /// Cancels the work and waits for its current transaction to end; aborts it past the budget
    /// (an aborted transaction rolls back). A work that panicked while it stopped is logged with
    /// its panic.
    async fn stop(mut self, name: LeadershipName, stop_timeout: Duration) {
        self.child.cancel();
        match tokio::time::timeout(stop_timeout, &mut self.handle).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                tracing::error!(target: LOG_TARGET, %name, error = %err, "the work failed while it stopped");
            }
            Err(_elapsed) => {
                tracing::warn!(target: LOG_TARGET, %name, ?stop_timeout, "the work did not stop within the budget; aborted");
                self.handle.abort();
                let _aborted = (&mut self.handle).await;
            }
        }
    }
}

impl Drop for ActiveWork {
    /// A dropped worker never outlives the loop.
    fn drop(&mut self) {
        self.child.cancel();
        self.handle.abort();
    }
}

fn coordination(context: &str, err: ClusterError) -> DomainError {
    DomainError::Coordination(format!("{context}: {err}"), Some(Cause::new(err)))
}

#[cfg(test)]
#[path = "cluster_tests.rs"]
mod tests;
