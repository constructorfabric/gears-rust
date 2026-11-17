//! Where Deletion runs: the leader's job, which runs every marked erasure
//! until it is done and removes what is past the retention periods.

use std::collections::BTreeSet;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cluster_sdk::{
    ClusterClient, ClusterError, ClusterProfile as _, LeaderElectionCapability, LeaderElectionV1,
};
use futures::FutureExt as _;
use time::OffsetDateTime;
use tokio_retry::strategy::{ExponentialBackoff, jitter};
use tokio_util::sync::CancellationToken;
use toolkit::client_hub::ClientHub;
use uuid::Uuid;

use crate::config::DeletionConfig;
use crate::domain::deletion::{Deletion, DeletionStore, ERASURE_WAIT, Erasure, RetentionPeriods};
use crate::domain::error::DomainError;
use crate::domain::subject_settings::SubjectKey;
use crate::domain::tenant_exit::{Exit, TenantExit, TenantStatus};
use crate::infra::panics::panic_message;

/// How many marked erasures one run of the job takes.
const BATCH: u64 = 100;

/// The shortest and the longest pause before the leader election is tried
/// again.
const RETRY_FIRST: Duration = Duration::from_secs(5);
const RETRY_LONGEST: Duration = Duration::from_mins(5);

/// Log how one erasure run ended. A failed run leaves the mark, so the
/// leader's job runs it again. A store that did not answer is a warning; a
/// missing processing identity, said once at start, is debug; a failure a
/// rerun does not cure, such as a defect of the stored profile or a refusal,
/// is logged as an error, as the erasure stays stuck until someone fixes it.
#[expect(
    clippy::cognitive_complexity,
    reason = "five log lines; the tracing macros count toward the complexity"
)]
fn log_erasure(key: SubjectKey, ended: Result<Erasure, DomainError>) {
    match ended {
        Ok(Erasure::Done) => tracing::info!(
            tenant_id = %key.tenant_id,
            subject_id = %key.subject_id,
            "construct: erasure done"
        ),
        Ok(Erasure::NotUnderWay) => tracing::debug!(
            tenant_id = %key.tenant_id,
            subject_id = %key.subject_id,
            "construct: no erasure under way"
        ),
        // Said once at start: without a processing identity no erasure runs.
        Err(DomainError::NotConfigured(_)) => tracing::debug!(
            tenant_id = %key.tenant_id,
            subject_id = %key.subject_id,
            "construct: the erasure waits for a processing identity"
        ),
        Err(err @ (DomainError::Unavailable(_) | DomainError::Database(_))) => tracing::warn!(
            tenant_id = %key.tenant_id,
            subject_id = %key.subject_id,
            error = %err,
            "construct: an erasure stopped; it stays marked and the next run goes on"
        ),
        Err(err) => tracing::error!(
            tenant_id = %key.tenant_id,
            subject_id = %key.subject_id,
            error = %err,
            "construct: an erasure failed in a way a rerun does not cure; it stays marked and \
             the leader keeps trying"
        ),
    }
}

/// Log how one tenant's retention sweep ended. Without a processing identity
/// every sweep ends so; the gear says it once at start.
#[expect(
    clippy::cognitive_complexity,
    reason = "three log lines; the tracing macros count toward the complexity"
)]
fn log_retention(
    tenant_id: Uuid,
    ended: Result<Result<(), DomainError>, Box<dyn std::any::Any + Send>>,
) {
    match ended {
        Ok(Ok(())) => {}
        Ok(Err(DomainError::NotConfigured(_))) => tracing::debug!(
            %tenant_id,
            "construct: retention waits for a processing identity"
        ),
        Ok(Err(err)) => tracing::warn!(
            %tenant_id,
            error = %err,
            "construct: retention stopped for the tenant; the next run goes on"
        ),
        Err(panic) => tracing::error!(
            %tenant_id,
            panic = panic_message(panic.as_ref()),
            "construct: retention panicked for the tenant; the next run goes on"
        ),
    }
}

/// Log how one run of a tenant's exit ended, or why it could not ask about
/// the tenant. A failed run leaves the tenant kept, so the next run goes on.
/// What did not answer is a warning; a refusal or a defect, such as graph
/// storage's refusal when the deployment did not grant Construct the
/// offboarding, is an error, as the tenant stays until someone fixes it. A
/// tenant the resolver does not know is a warning on every run: its data
/// stays until an operator removes it.
///
/// @cpt-dod:cpt-cf-construct-dod-retention-tenant-exit:p2
#[expect(
    clippy::cognitive_complexity,
    reason = "one log line per outcome; the tracing macros count toward the complexity"
)]
fn log_exit(tenant_id: Uuid, ended: &Result<Exit, DomainError>) {
    match ended {
        Ok(Exit::Kept(TenantStatus::Unknown)) => tracing::warn!(
            %tenant_id,
            "construct: the tenant resolver does not know the tenant, so nothing says it was \
             deleted; its data stays until an operator removes it"
        ),
        Ok(Exit::Done) => tracing::info!(%tenant_id, "construct: tenant exit done"),
        Ok(Exit::UnderWay) => tracing::info!(
            %tenant_id,
            "construct: graph storage is removing the deleted tenant's graph and its rows here \
             are removed; the next run asks again"
        ),
        // A tenant that is not deleted; or no resolver or no identity, each
        // said once, not for every tenant on every run.
        Ok(Exit::Kept(_)) | Err(DomainError::NotConfigured(_)) => {}
        Err(err @ (DomainError::Unavailable(_) | DomainError::Database(_))) => tracing::warn!(
            %tenant_id,
            error = %err,
            "construct: whether the tenant is deleted could not be read; the next run asks again"
        ),
        Err(err) => tracing::error!(
            %tenant_id,
            error = %err,
            "construct: whether the tenant is deleted could not be read, in a way a rerun does \
             not cure; a deleted tenant is not removed until someone fixes it"
        ),
        Ok(Exit::Stopped(err @ (DomainError::Unavailable(_) | DomainError::Database(_)))) => {
            tracing::warn!(
                %tenant_id,
                error = %err,
                "construct: tenant exit stopped for the tenant; the next run goes on"
            );
        }
        Ok(Exit::Stopped(err)) => tracing::error!(
            %tenant_id,
            error = %err,
            "construct: tenant exit failed in a way a rerun does not cure; the tenant stays and \
             the leader keeps trying"
        ),
    }
}

/// The tenants one retention run takes: those it sweeps for retention, and
/// those it checks for an exit, which include them.
struct RunTenants {
    swept: BTreeSet<Uuid>,
    checked: BTreeSet<Uuid>,
}

/// The leader's job: the marked erasures, and retention.
///
/// @cpt-dod:cpt-cf-construct-dod-retention-job:p2
pub struct DeletionJob {
    pub deletion: Arc<Deletion>,
    pub store: Arc<dyn DeletionStore>,
    pub periods: Arc<dyn RetentionPeriods>,
    /// How often the marked erasures are run.
    pub interval: Duration,
    /// How often retention runs; periods are counted in days, so far less
    /// often than the erasures.
    pub retention_interval: Duration,
    /// Tenant exit, which each retention run checks every kept tenant for.
    pub exit: Arc<TenantExit>,
}

impl DeletionJob {
    /// The job with the intervals `config` sets.
    #[must_use]
    pub fn new(
        deletion: Arc<Deletion>,
        store: Arc<dyn DeletionStore>,
        periods: Arc<dyn RetentionPeriods>,
        exit: Arc<TenantExit>,
        config: &DeletionConfig,
    ) -> Self {
        Self {
            deletion,
            store,
            periods,
            interval: Duration::from_secs(config.job_interval_secs.get()),
            retention_interval: Duration::from_secs(config.retention_interval_secs.get()),
            exit,
        }
    }

    /// One run: every erasure marked at least [`ERASURE_WAIT`] ago and still
    /// under way, up to [`BATCH`], then each tenant with a retention period.
    /// What fails is logged and left for the next run.
    pub async fn run_once(&self) {
        self.run_erasures().await;
        self.run_retention().await;
    }

    /// Every erasure marked at least [`ERASURE_WAIT`] ago, up to [`BATCH`],
    /// the longest untried first. A younger one would only sleep out its
    /// wait here and hold up the batch. Each attempt is noted before it runs,
    /// so an erasure that keeps failing goes behind the others.
    async fn run_erasures(&self) {
        // @cpt-begin:cpt-cf-construct-algo-erasure-rerun:p1:inst-rerun-run
        let marked_before = OffsetDateTime::now_utc() - ERASURE_WAIT;
        match self.store.under_way(marked_before, BATCH).await {
            Ok(keys) => {
                for key in keys {
                    self.run_erasure(key).await;
                }
            }
            Err(err) => tracing::warn!(
                error = %err,
                "construct: the marked erasures could not be listed; the next run tries again"
            ),
        }
        // @cpt-end:cpt-cf-construct-algo-erasure-rerun:p1:inst-rerun-run
    }

    /// Note the attempt, then run the erasure.
    async fn run_erasure(&self, key: SubjectKey) {
        // @cpt-begin:cpt-cf-construct-algo-erasure-rerun:p1:inst-rerun-attempt
        if let Err(err) = self.store.attempted(key, OffsetDateTime::now_utc()).await {
            tracing::warn!(
                tenant_id = %key.tenant_id,
                subject_id = %key.subject_id,
                error = %err,
                "construct: the erasure's attempt could not be noted; it runs anyway"
            );
        }
        // @cpt-end:cpt-cf-construct-algo-erasure-rerun:p1:inst-rerun-attempt
        // A panic ends this erasure's run only; the job goes on to the next.
        match AssertUnwindSafe(self.deletion.erase(key))
            .catch_unwind()
            .await
        {
            Ok(ended) => log_erasure(key, ended),
            Err(panic) => tracing::error!(
                tenant_id = %key.tenant_id,
                subject_id = %key.subject_id,
                panic = panic_message(panic.as_ref()),
                "construct: an erasure panicked; it stays marked and the leader runs it again"
            ),
        }
    }

    /// Each kept tenant: a deleted one exits; any other with a period is
    /// swept for retention.
    async fn run_retention(&self) {
        // @cpt-begin:cpt-cf-construct-algo-retention-expire:p2:inst-expire-tenants
        let RunTenants { swept, checked } = self.tenants().await;
        for tenant_id in checked {
            // A deleted tenant leaves rather than being swept: its data goes
            // whatever its period.
            if self.exit_if_deleted(tenant_id).await || !swept.contains(&tenant_id) {
                continue;
            }
            match self.periods.period(tenant_id).await {
                // A panic ends this tenant's sweep only; the job goes on to
                // the next tenant.
                Ok(Some(period)) => log_retention(
                    tenant_id,
                    AssertUnwindSafe(self.deletion.expire(tenant_id, period))
                        .catch_unwind()
                        .await,
                ),
                Ok(None) => {}
                Err(err) => tracing::warn!(
                    %tenant_id,
                    error = %err,
                    "construct: the tenant's retention period could not be read"
                ),
            }
        }
        // @cpt-end:cpt-cf-construct-algo-retention-expire:p2:inst-expire-tenants
    }

    /// Run the tenant's exit; whether the platform has deleted the tenant,
    /// so retention leaves it alone. A tenant whose status cannot be read is
    /// not taken as deleted: retention sweeps it as before.
    async fn exit_if_deleted(&self, tenant_id: Uuid) -> bool {
        // A panic ends this tenant's exit only; the job goes on to the next.
        match AssertUnwindSafe(self.exit.run(tenant_id))
            .catch_unwind()
            .await
        {
            Ok(ended) => {
                log_exit(tenant_id, &ended);
                matches!(ended, Ok(Exit::UnderWay | Exit::Done | Exit::Stopped(_)))
            }
            Err(panic) => {
                tracing::error!(
                    %tenant_id,
                    panic = panic_message(panic.as_ref()),
                    "construct: tenant exit panicked for the tenant; the next run goes on"
                );
                false
            }
        }
    }

    /// The tenants retention sweeps: those given a period of their own and,
    /// with a default period, every tenant Construct keeps; and the tenants
    /// tenant exit checks: those, and every tenant Construct keeps.
    async fn tenants(&self) -> RunTenants {
        let mut swept = self.periods.listed();
        let mut checked = swept.clone();
        match self.store.tenants().await {
            Ok(kept) => {
                if self.periods.has_default() {
                    swept.extend(kept.iter().copied());
                }
                checked.extend(kept);
            }
            Err(err) => tracing::warn!(
                error = %err,
                "construct: the tenants could not be listed; only the listed ones are swept or \
                 checked for an exit"
            ),
        }
        RunTenants { swept, checked }
    }

    /// Run the job until `cancel` fires: the marked erasures every `interval`
    /// and retention every `retention_interval`, each in a loop of its own,
    /// so a long retention sweep does not hold off an erasure.
    ///
    /// NOT cancel-safe for the runs in flight: they stop where they are, and
    /// the next leader's runs go on from what is left.
    pub async fn run(self: Arc<Self>, cancel: CancellationToken) {
        tokio::join!(
            self.every_interval(&cancel, self.interval, || self.run_erasures()),
            self.every_interval(&cancel, self.retention_interval, || self.run_retention()),
        );
    }

    /// Run `step` at once, then `interval` after each run ends, until
    /// `cancel` fires.
    async fn every_interval<F, Fut>(&self, cancel: &CancellationToken, interval: Duration, step: F)
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        loop {
            // A panic outside one erasure or one tenant, such as in a listing,
            // ends this run only; the next comes after the interval.
            tokio::select! {
                () = cancel.cancelled() => return,
                ran = AssertUnwindSafe(step()).catch_unwind() => {
                    if let Err(panic) = ran {
                        tracing::error!(
                            panic = panic_message(panic.as_ref()),
                            "construct: a run of the deletion job panicked; the next run goes on"
                        );
                    }
                }
            }
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(interval) => {}
            }
        }
    }
}

/// Runs work on one instance of the cluster at a time.
#[async_trait]
pub trait Leadership: Send + Sync {
    /// Run `job` while this instance leads, until `cancel` fires.
    async fn lead(&self, job: Arc<DeletionJob>, cancel: CancellationToken);
}

/// The scope of Construct's elections on the cluster gear.
const SCOPE: &str = "construct";
/// The election of the deletion job.
const ELECTION: &str = "deletion";
/// How long a job that lost the lead may take to stop.
const STOP: Duration = Duration::from_secs(10);

/// The cluster profile Construct's leader election resolves.
#[derive(Debug, Clone, Copy, Default)]
pub struct ConstructProfile;

impl cluster_sdk::ClusterProfile for ConstructProfile {
    const NAME: &'static str = "construct";
}

/// [`Leadership`] on the platform cluster gear's leader election, with the
/// `construct` profile. A deployment without it runs no job, and says so: the
/// election is not resolved then, since a resolve records the profile as a
/// requirement of the whole host's readiness even when it is not bound.
pub struct ClusterLeadership {
    hub: Arc<ClientHub>,
}

/// The pauses before the next tries of the leader election: from
/// [`RETRY_FIRST`], doubling up to [`RETRY_LONGEST`].
fn election_backoff() -> ExponentialBackoff {
    // The base is raised to the power of the try, so with a base of 2 a
    // factor of half the first pause starts there.
    ExponentialBackoff::from_millis(2)
        .factor(RETRY_FIRST.as_secs() * 500)
        .max_delay(RETRY_LONGEST)
}

/// Sleep for the next pause of `backoff`, with up to as much again at random,
/// so instances that failed together do not come back together; false when
/// `cancel` fires first.
async fn pause(backoff: &mut ExponentialBackoff, cancel: &CancellationToken) -> bool {
    let pause = backoff.next().unwrap_or(RETRY_LONGEST);
    tokio::select! {
        () = cancel.cancelled() => false,
        () = tokio::time::sleep(pause.saturating_add(jitter(pause))) => true,
    }
}

impl ClusterLeadership {
    #[must_use]
    pub fn new(hub: Arc<ClientHub>) -> Self {
        Self { hub }
    }

    /// Whether the host binds the `construct` profile: false without a
    /// cluster client or when the client says the profile is not bound. Any
    /// other answer, a cluster that does not answer yet included, counts as
    /// bound, and the election is resolved and tried again until it answers.
    async fn bound(&self, cancel: &CancellationToken) -> bool {
        let Some(client) = self.hub.try_get::<dyn ClusterClient>() else {
            return false;
        };
        tokio::select! {
            () = cancel.cancelled() => false,
            descriptor = client.descriptor(ConstructProfile::NAME) => {
                !matches!(descriptor, Err(ClusterError::ProfileNotBound { .. }))
            }
        }
    }

    /// The election, resolved once the cluster gear answers; none when
    /// `cancel` fires first.
    async fn election(&self, cancel: &CancellationToken) -> Option<LeaderElectionV1> {
        let mut backoff = election_backoff();
        loop {
            let resolving = LeaderElectionV1::resolver(&self.hub)
                .profile(ConstructProfile)
                .require(LeaderElectionCapability::Linearizable)
                .resolve();
            let resolved = tokio::select! {
                () = cancel.cancelled() => return None,
                resolved = resolving => resolved.and_then(|election| election.scoped(SCOPE)),
            };
            match resolved {
                Ok(election) => return Some(election),
                Err(err) => tracing::warn!(
                    error = %err,
                    "construct: no leader election for the deletion job; no erasure runs and \
                     nothing is removed for retention until there is one"
                ),
            }
            if !pause(&mut backoff, cancel).await {
                return None;
            }
        }
    }
}

#[async_trait]
impl Leadership for ClusterLeadership {
    async fn lead(&self, job: Arc<DeletionJob>, cancel: CancellationToken) {
        // @cpt-begin:cpt-cf-construct-algo-erasure-rerun:p1:inst-rerun-leader
        if !self.bound(&cancel).await {
            if !cancel.is_cancelled() {
                tracing::warn!(
                    profile = ConstructProfile::NAME,
                    "construct: the host binds no `construct` cluster profile; the deletion job \
                     does not run, so no erasure runs and nothing is removed for retention"
                );
            }
            return;
        }
        let mut backoff = election_backoff();
        loop {
            let Some(election) = self.election(&cancel).await else {
                return;
            };
            let watch = tokio::select! {
                () = cancel.cancelled() => return,
                watch = election.elect(ELECTION) => watch,
            };
            match watch {
                Ok(watch) => {
                    backoff = election_backoff();
                    let job = Arc::clone(&job);
                    // Dropping the watch on shutdown stops the job it runs.
                    // The job starts when this instance gains the lead and is
                    // cancelled when it loses it.
                    tokio::select! {
                        () = cancel.cancelled() => return,
                        () = watch.run_while_leader(STOP, move |token| {
                            let job = Arc::clone(&job);
                            async move {
                                tracing::info!("construct: this instance leads the deletion job");
                                job.run(token).await;
                                tracing::info!(
                                    "construct: this instance no longer runs the deletion job"
                                );
                            }
                        }) => {}
                    }
                }
                Err(err) => tracing::warn!(
                    error = %err,
                    "construct: the deletion job's election could not be joined"
                ),
            }
            if !pause(&mut backoff, &cancel).await {
                return;
            }
        }
        // @cpt-end:cpt-cf-construct-algo-erasure-rerun:p1:inst-rerun-leader
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::sync::atomic::Ordering;

    use serde_json::json;

    use super::*;
    use crate::config::RetentionConfig;
    use crate::domain::deletion::{DeletionPorts, SubjectsPage};
    use crate::domain::profile::Category;
    use crate::infra::retention_periods::ConfigRetentionPeriods;
    use crate::test_support::{
        FixedDirectory, MemoryDeletionStore, MemoryProfile, MemoryWriter, ScriptedRoots,
        profile_writes, stored_fact,
    };

    struct Job {
        key: SubjectKey,
        profile: Arc<MemoryProfile>,
        store: Arc<MemoryDeletionStore>,
        roots: Arc<ScriptedRoots>,
        deletion: Arc<Deletion>,
    }

    /// A subject with one fact stored at second 10, listed in its tenant.
    fn job() -> Job {
        let key = SubjectKey {
            tenant_id: Uuid::new_v4(),
            subject_id: Uuid::new_v4(),
        };
        let profile = MemoryProfile::with(vec![stored_fact(
            Category::Skill,
            "skills",
            json!({ "name": "Rust" }),
            10,
        )]);
        let store = Arc::new(MemoryDeletionStore::default());
        let roots = Arc::new(ScriptedRoots::default());
        let deletion = Arc::new(Deletion::new(DeletionPorts {
            writes: Arc::new(profile_writes(
                Arc::clone(&profile),
                MemoryWriter::new(Arc::clone(&profile), 0),
                Some(Uuid::new_v4()),
            )),
            roots: Arc::clone(&roots) as _,
            directory: Arc::new(FixedDirectory {
                pages: vec![SubjectsPage {
                    subjects: vec![key.subject_id],
                    next: None,
                }],
                ..FixedDirectory::default()
            }),
            store: Arc::clone(&store) as _,
            reviews: Arc::default() as Arc<crate::test_support::MemoryReviews>,
            review_events: Arc::default() as Arc<crate::test_support::RecordingReviewEvents>,
            events: Arc::default() as Arc<crate::test_support::RecordingDeletionEvents>,
        }));
        Job {
            key,
            profile,
            store,
            roots,
            deletion,
        }
    }

    fn periods(retention: RetentionConfig) -> Arc<ConfigRetentionPeriods> {
        Arc::new(ConfigRetentionPeriods::new(retention))
    }

    #[tokio::test]
    async fn the_job_runs_the_erasures_past_their_wait_and_leaves_a_fresh_one_for_later() {
        let job = job();
        let fresh = SubjectKey {
            tenant_id: Uuid::new_v4(),
            subject_id: Uuid::new_v4(),
        };
        job.store
            .mark_at(job.key, OffsetDateTime::now_utc() - ERASURE_WAIT * 2);
        job.store.mark_at(fresh, OffsetDateTime::now_utc());
        let runner = DeletionJob {
            deletion: Arc::clone(&job.deletion),
            store: Arc::clone(&job.store) as _,
            periods: periods(RetentionConfig::default()),
            interval: Duration::from_secs(60),
            retention_interval: Duration::from_secs(60),
            exit: crate::test_support::tenant_exit_without_resolver(Arc::clone(&job.store)),
        };

        runner.run_once().await;

        assert_eq!(job.store.finished(), [job.key]);
        assert!(job.profile.live().is_empty());
        assert!(
            job.store.is_marked(fresh),
            "a later run takes it once its wait is over"
        );
    }

    #[tokio::test]
    async fn the_job_sweeps_the_listed_tenants_and_with_a_default_every_tenant_held() {
        let job = job();
        let held = Uuid::new_v4();
        job.store.tenants.lock().unwrap().insert(held);
        let days = NonZeroU32::new(30).unwrap();

        let listed_only = DeletionJob {
            deletion: Arc::clone(&job.deletion),
            store: Arc::clone(&job.store) as _,
            periods: periods(RetentionConfig {
                default_days: None,
                tenants: [(job.key.tenant_id, days)].into(),
            }),
            interval: Duration::from_secs(60),
            retention_interval: Duration::from_secs(60),
            exit: crate::test_support::tenant_exit_without_resolver(Arc::clone(&job.store)),
        };
        listed_only.run_once().await;
        let swept: Vec<Uuid> = job
            .store
            .expired
            .lock()
            .unwrap()
            .iter()
            .map(|(tenant, _)| *tenant)
            .collect();
        assert_eq!(swept, [job.key.tenant_id]);
        assert!(job.profile.live().is_empty(), "the old fact is removed");

        job.store.expired.lock().unwrap().clear();
        let with_default = DeletionJob {
            periods: periods(RetentionConfig {
                default_days: Some(days),
                tenants: [(job.key.tenant_id, days)].into(),
            }),
            ..listed_only
        };
        with_default.run_once().await;
        let swept: BTreeSet<Uuid> = job
            .store
            .expired
            .lock()
            .unwrap()
            .iter()
            .map(|(tenant, _)| *tenant)
            .collect();
        assert_eq!(swept, BTreeSet::from([job.key.tenant_id, held]));
    }

    #[tokio::test]
    async fn without_a_period_nothing_is_swept() {
        let job = job();
        job.store.tenants.lock().unwrap().insert(job.key.tenant_id);
        let runner = DeletionJob {
            deletion: Arc::clone(&job.deletion),
            store: Arc::clone(&job.store) as _,
            periods: periods(RetentionConfig::default()),
            interval: Duration::from_secs(60),
            retention_interval: Duration::from_secs(60),
            exit: crate::test_support::tenant_exit_without_resolver(Arc::clone(&job.store)),
        };

        runner.run_once().await;

        assert!(job.store.expired.lock().unwrap().is_empty());
        assert_eq!(job.profile.live().len(), 1);
    }

    fn another_subject() -> SubjectKey {
        SubjectKey {
            tenant_id: Uuid::new_v4(),
            subject_id: Uuid::new_v4(),
        }
    }

    /// Wait until `done` holds, for at most ten seconds.
    async fn until(what: &str, done: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !done() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{what}"));
    }

    #[tokio::test]
    async fn a_tenant_that_fails_does_not_stop_the_others() {
        let job = job();
        // The tenants are swept in order; this one sorts first.
        let other = Uuid::from_u128(1);
        *job.store.failing_tenant.lock().unwrap() = Some(other);
        let days = NonZeroU32::new(30).unwrap();
        let runner = DeletionJob {
            deletion: Arc::clone(&job.deletion),
            store: Arc::clone(&job.store) as _,
            periods: periods(RetentionConfig {
                default_days: None,
                tenants: [(other, days), (job.key.tenant_id, days)].into(),
            }),
            interval: Duration::from_secs(60),
            retention_interval: Duration::from_secs(60),
            exit: crate::test_support::tenant_exit_without_resolver(Arc::clone(&job.store)),
        };

        runner.run_once().await;

        let swept: Vec<Uuid> = job
            .store
            .expired
            .lock()
            .unwrap()
            .iter()
            .map(|(tenant, _)| *tenant)
            .collect();
        assert_eq!(swept, [job.key.tenant_id]);
        assert!(job.profile.live().is_empty());
    }

    #[tokio::test]
    async fn without_the_held_tenants_only_the_listed_ones_are_swept() {
        let job = job();
        job.store.tenants.lock().unwrap().insert(Uuid::new_v4());
        job.store
            .tenants_down
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let days = NonZeroU32::new(30).unwrap();
        let runner = DeletionJob {
            deletion: Arc::clone(&job.deletion),
            store: Arc::clone(&job.store) as _,
            periods: periods(RetentionConfig {
                default_days: Some(days),
                tenants: [(job.key.tenant_id, days)].into(),
            }),
            interval: Duration::from_secs(60),
            retention_interval: Duration::from_secs(60),
            exit: crate::test_support::tenant_exit_without_resolver(Arc::clone(&job.store)),
        };

        runner.run_once().await;

        let swept: Vec<Uuid> = job
            .store
            .expired
            .lock()
            .unwrap()
            .iter()
            .map(|(tenant, _)| *tenant)
            .collect();
        assert_eq!(swept, [job.key.tenant_id]);
    }

    #[test]
    fn the_election_pauses_start_at_the_first_and_double_up_to_the_longest() {
        let pauses: Vec<u64> = election_backoff()
            .take(9)
            .map(|pause| pause.as_secs())
            .collect();
        assert_eq!(pauses, [5, 10, 20, 40, 80, 160, 300, 300, 300]);
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn a_rerun_that_panics_is_logged_and_the_next_run_finishes_the_erasure() {
        let job = job();
        job.store
            .mark_at(job.key, OffsetDateTime::now_utc() - ERASURE_WAIT * 2);
        job.roots
            .panic_once
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let runner = DeletionJob {
            deletion: Arc::clone(&job.deletion),
            store: Arc::clone(&job.store) as _,
            periods: periods(RetentionConfig::default()),
            interval: Duration::from_secs(60),
            retention_interval: Duration::from_secs(60),
            exit: crate::test_support::tenant_exit_without_resolver(Arc::clone(&job.store)),
        };

        runner.run_once().await;

        assert!(logs_contain("an erasure panicked"));
        assert!(logs_contain(&job.key.subject_id.to_string()));
        assert!(job.store.is_marked(job.key), "the panicked run left it");
        runner.run_once().await;
        assert_eq!(job.store.finished(), [job.key]);
    }

    /// A directory that panics listing one tenant and lists no subject in
    /// any other; it notes each tenant it is asked about.
    struct PanicsListing {
        tenant_id: Uuid,
        asked: std::sync::Mutex<Vec<Uuid>>,
    }

    #[async_trait]
    impl crate::domain::deletion::ProfileDirectory for PanicsListing {
        async fn subjects(
            &self,
            ctx: &toolkit_security::SecurityContext,
            _cursor: Option<String>,
        ) -> Result<SubjectsPage, DomainError> {
            let tenant_id = ctx.subject_tenant_id();
            self.asked.lock().unwrap().push(tenant_id);
            assert_ne!(tenant_id, self.tenant_id, "listing this tenant panics");
            Ok(SubjectsPage {
                subjects: Vec::new(),
                next: None,
            })
        }
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn a_tenant_whose_sweep_panics_is_logged_and_the_next_tenant_is_swept() {
        let job = job();
        let (first, second) = {
            let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
            (a.min(b), a.max(b))
        };
        let directory = Arc::new(PanicsListing {
            tenant_id: first,
            asked: std::sync::Mutex::default(),
        });
        let deletion = Arc::new(Deletion::new(DeletionPorts {
            writes: Arc::new(profile_writes(
                Arc::clone(&job.profile),
                MemoryWriter::new(Arc::clone(&job.profile), 0),
                Some(Uuid::new_v4()),
            )),
            roots: Arc::clone(&job.roots) as _,
            directory: Arc::clone(&directory) as _,
            store: Arc::clone(&job.store) as _,
            reviews: Arc::default() as Arc<crate::test_support::MemoryReviews>,
            review_events: Arc::default() as Arc<crate::test_support::RecordingReviewEvents>,
            events: Arc::default() as Arc<crate::test_support::RecordingDeletionEvents>,
        }));
        let days = NonZeroU32::new(30).unwrap();
        let runner = DeletionJob {
            deletion,
            store: Arc::clone(&job.store) as _,
            periods: periods(RetentionConfig {
                default_days: None,
                tenants: [(first, days), (second, days)].into(),
            }),
            interval: Duration::from_secs(60),
            retention_interval: Duration::from_secs(60),
            exit: crate::test_support::tenant_exit_without_resolver(Arc::clone(&job.store)),
        };

        runner.run_once().await;

        assert_eq!(
            *directory.asked.lock().unwrap(),
            [first, second],
            "the tenants are swept in order, the second after the first panicked"
        );
        assert!(logs_contain("retention panicked for the tenant"));
        assert!(logs_contain(&first.to_string()));
        assert!(
            !logs_contain("a run of the deletion job panicked"),
            "the tenant's guard caught it, not the run's"
        );
    }

    /// The job over `job`'s stores with `retention`, run once a minute.
    fn runner(job: &Job, retention: RetentionConfig) -> DeletionJob {
        DeletionJob {
            deletion: Arc::clone(&job.deletion),
            store: Arc::clone(&job.store) as _,
            periods: periods(retention),
            interval: Duration::from_secs(60),
            retention_interval: Duration::from_secs(60),
            exit: crate::test_support::tenant_exit_without_resolver(Arc::clone(&job.store)),
        }
    }

    /// The tenants retention removed rows of, in order.
    fn swept(job: &Job) -> Vec<Uuid> {
        job.store
            .expired
            .lock()
            .unwrap()
            .iter()
            .map(|(tenant, _)| *tenant)
            .collect()
    }

    #[test]
    fn the_job_takes_its_intervals_from_the_config() {
        let job = job();
        let config: DeletionConfig = serde_json::from_value(json!({
            "job_interval_secs": 45,
            "retention_interval_secs": 7200,
        }))
        .unwrap();

        let built = DeletionJob::new(
            Arc::clone(&job.deletion),
            Arc::clone(&job.store) as _,
            periods(RetentionConfig::default()),
            crate::test_support::tenant_exit_without_resolver(Arc::clone(&job.store)),
            &config,
        );

        assert_eq!(built.interval, Duration::from_secs(45));
        assert_eq!(built.retention_interval, Duration::from_secs(7200));
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn erasures_that_cannot_be_listed_leave_retention_to_run() {
        let job = job();
        job.store
            .mark_at(job.key, OffsetDateTime::now_utc() - ERASURE_WAIT * 2);
        job.store.under_way_down.store(true, Ordering::SeqCst);
        let days = NonZeroU32::new(30).unwrap();
        let tenant = Uuid::new_v4();

        runner(
            &job,
            RetentionConfig {
                default_days: None,
                tenants: [(tenant, days)].into(),
            },
        )
        .run_once()
        .await;

        assert!(logs_contain("the marked erasures could not be listed"));
        assert!(job.store.is_marked(job.key), "the next run takes it");
        assert_eq!(swept(&job), [tenant]);
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn an_attempt_that_cannot_be_noted_still_runs_the_erasure() {
        let job = job();
        let other = another_subject();
        job.store
            .mark_at(job.key, OffsetDateTime::now_utc() - ERASURE_WAIT * 2);
        job.store
            .mark_at(other, OffsetDateTime::now_utc() - ERASURE_WAIT * 2);
        job.store.attempts_down.store(true, Ordering::SeqCst);

        runner(&job, RetentionConfig::default()).run_once().await;

        assert!(logs_contain("the erasure's attempt could not be noted"));
        let finished = job.store.finished();
        assert_eq!(finished.len(), 2, "{finished:?}");
        assert!(finished.contains(&job.key) && finished.contains(&other));
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn a_period_that_cannot_be_read_skips_only_its_tenant() {
        let job = job();
        let (first, second) = {
            let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
            (a.min(b), a.max(b))
        };
        let runner = DeletionJob {
            periods: Arc::new(FailsFor {
                failing: first,
                others: second,
            }),
            ..runner(&job, RetentionConfig::default())
        };

        runner.run_once().await;

        assert!(logs_contain(
            "the tenant's retention period could not be read"
        ));
        assert_eq!(swept(&job), [second], "the next tenant is swept");
    }

    /// Retention periods that fail for `failing` and give `others` thirty
    /// days; both are listed.
    struct FailsFor {
        failing: Uuid,
        others: Uuid,
    }

    #[async_trait]
    impl RetentionPeriods for FailsFor {
        async fn period(&self, tenant_id: Uuid) -> Result<Option<Duration>, DomainError> {
            if tenant_id == self.failing {
                return Err(DomainError::Unavailable("settings are down".to_owned()));
            }
            Ok(Some(Duration::from_hours(30 * 24)))
        }

        fn listed(&self) -> BTreeSet<Uuid> {
            [self.failing, self.others].into()
        }

        fn has_default(&self) -> bool {
            false
        }
    }

    /// Retention periods whose first read panics, then give no period.
    struct PanicsOnce {
        tenant_id: Uuid,
        reads: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl RetentionPeriods for PanicsOnce {
        async fn period(&self, _tenant_id: Uuid) -> Result<Option<Duration>, DomainError> {
            let read = self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            assert!(read > 0, "the first read of the period panics");
            Ok(None)
        }

        fn listed(&self) -> BTreeSet<Uuid> {
            [self.tenant_id].into()
        }

        fn has_default(&self) -> bool {
            false
        }
    }

    #[tracing_test::traced_test]
    #[tokio::test(start_paused = true)]
    async fn a_run_that_panics_is_logged_and_the_job_runs_again_after_the_interval() {
        let job = job();
        let periods = Arc::new(PanicsOnce {
            tenant_id: job.key.tenant_id,
            reads: std::sync::atomic::AtomicUsize::new(0),
        });
        let runner = Arc::new(DeletionJob {
            deletion: Arc::clone(&job.deletion),
            store: Arc::clone(&job.store) as _,
            periods: Arc::clone(&periods) as _,
            interval: Duration::from_secs(60),
            retention_interval: Duration::from_secs(60),
            exit: crate::test_support::tenant_exit_without_resolver(Arc::clone(&job.store)),
        });
        let reads = || periods.reads.load(std::sync::atomic::Ordering::SeqCst);
        let cancel = CancellationToken::new();
        let running = tokio::spawn(Arc::clone(&runner).run(cancel.clone()));

        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(reads(), 1);
        assert!(logs_contain("a run of the deletion job panicked"));
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert_eq!(reads(), 2, "the job is still running after the panic");

        cancel.cancel();
        running.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn the_job_runs_at_once_then_every_interval_until_cancelled() {
        let job = job();
        let days = NonZeroU32::new(30).unwrap();
        let runner = Arc::new(DeletionJob {
            deletion: Arc::clone(&job.deletion),
            store: Arc::clone(&job.store) as _,
            periods: periods(RetentionConfig {
                default_days: None,
                tenants: [(job.key.tenant_id, days)].into(),
            }),
            interval: Duration::from_secs(60),
            retention_interval: Duration::from_secs(60),
            exit: crate::test_support::tenant_exit_without_resolver(Arc::clone(&job.store)),
        });
        let runs = || job.store.expired.lock().unwrap().len();
        let cancel = CancellationToken::new();
        let running = tokio::spawn(Arc::clone(&runner).run(cancel.clone()));

        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(runs(), 1, "the first run starts at once");
        tokio::time::sleep(Duration::from_secs(30)).await;
        assert_eq!(runs(), 1, "the next waits for the interval");
        tokio::time::sleep(Duration::from_secs(30)).await;
        assert_eq!(runs(), 2);

        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), running)
            .await
            .expect("the job ends when cancelled")
            .unwrap();
        assert_eq!(runs(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn retention_runs_on_its_own_longer_interval() {
        let job = job();
        let days = NonZeroU32::new(30).unwrap();
        let runner = Arc::new(DeletionJob {
            deletion: Arc::clone(&job.deletion),
            store: Arc::clone(&job.store) as _,
            periods: periods(RetentionConfig {
                default_days: None,
                tenants: [(job.key.tenant_id, days)].into(),
            }),
            interval: Duration::from_secs(60),
            retention_interval: Duration::from_secs(600),
            exit: crate::test_support::tenant_exit_without_resolver(Arc::clone(&job.store)),
        });
        let sweeps = || job.store.expired.lock().unwrap().len();
        let cancel = CancellationToken::new();
        let running = tokio::spawn(Arc::clone(&runner).run(cancel.clone()));

        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(sweeps(), 1, "the first sweep starts at once");
        tokio::time::sleep(Duration::from_secs(120)).await;
        assert_eq!(sweeps(), 1, "not on the reruns' interval");
        tokio::time::sleep(Duration::from_secs(480)).await;
        assert_eq!(sweeps(), 2);

        cancel.cancel();
        running.await.unwrap();
    }

    /// Retention periods whose read never ends, for a sweep that hangs.
    struct Hanging(Uuid);

    #[async_trait]
    impl RetentionPeriods for Hanging {
        async fn period(&self, _tenant_id: Uuid) -> Result<Option<Duration>, DomainError> {
            std::future::pending().await
        }

        fn listed(&self) -> BTreeSet<Uuid> {
            [self.0].into()
        }

        fn has_default(&self) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn a_retention_sweep_that_does_not_end_does_not_hold_off_an_erasure() {
        let job = job();
        job.store
            .mark_at(job.key, OffsetDateTime::now_utc() - ERASURE_WAIT * 2);
        let runner = Arc::new(DeletionJob {
            deletion: Arc::clone(&job.deletion),
            store: Arc::clone(&job.store) as _,
            periods: Arc::new(Hanging(job.key.tenant_id)),
            interval: Duration::from_secs(60),
            retention_interval: Duration::from_secs(60),
            exit: crate::test_support::tenant_exit_without_resolver(Arc::clone(&job.store)),
        });
        let cancel = CancellationToken::new();
        let running = tokio::spawn(Arc::clone(&runner).run(cancel.clone()));

        until("the erasure runs beside the sweep", || {
            job.store.finished() == [job.key]
        })
        .await;

        cancel.cancel();
        running.await.unwrap();
    }

    /// A host that binds no `construct` profile, with no cluster client or
    /// one that binds other profiles only, resolves no election: a resolve
    /// would record the profile as a requirement of the host's readiness and
    /// keep every gear of the host from being ready.
    #[tracing_test::traced_test]
    #[tokio::test]
    async fn a_host_without_the_construct_profile_resolves_no_election_and_runs_no_job() {
        for hub in [ClientHub::new(), {
            let hub = ClientHub::new();
            cluster_sdk::test_support::StubClusterClient::for_profile("oagw").register(&hub);
            hub
        }] {
            let job = job();
            let runner = Arc::new(DeletionJob {
                deletion: Arc::clone(&job.deletion),
                store: Arc::clone(&job.store) as _,
                periods: periods(RetentionConfig::default()),
                interval: Duration::from_secs(60),
                retention_interval: Duration::from_secs(60),
                exit: crate::test_support::tenant_exit_without_resolver(Arc::clone(&job.store)),
            });

            tokio::time::timeout(
                Duration::from_secs(5),
                ClusterLeadership::new(Arc::new(hub)).lead(runner, CancellationToken::new()),
            )
            .await
            .expect("the leadership ends at once");

            assert!(job.store.expired.lock().unwrap().is_empty(), "no job ran");
        }
        assert!(logs_contain("binds no `construct` cluster profile"));
        assert!(
            !logs_contain("no leader election for the deletion job")
                && !logs_contain("election could not be joined"),
            "no election was resolved"
        );
    }

    /// A cluster that binds the profile but does not answer yet is resolved
    /// and tried again.
    #[tracing_test::traced_test]
    #[tokio::test(start_paused = true)]
    async fn a_cluster_that_does_not_answer_yet_is_tried_again_after_a_pause() {
        let job = job();
        let runner = Arc::new(DeletionJob {
            deletion: Arc::clone(&job.deletion),
            store: Arc::clone(&job.store) as _,
            periods: periods(RetentionConfig::default()),
            interval: Duration::from_secs(60),
            retention_interval: Duration::from_secs(60),
            exit: crate::test_support::tenant_exit_without_resolver(Arc::clone(&job.store)),
        });
        let hub = ClientHub::new();
        cluster_sdk::test_support::StubClusterClient::for_profile("construct")
            .without_descriptor()
            .register(&hub);
        let leadership = ClusterLeadership::new(Arc::new(hub));
        let cancel = CancellationToken::new();
        let tries = || {
            let tries = std::cell::Cell::new(0);
            logs_assert(|lines| {
                tries.set(
                    lines
                        .iter()
                        .filter(|line| {
                            line.contains("no leader election for the deletion job")
                                || line.contains("election could not be joined")
                        })
                        .count(),
                );
                Ok(())
            });
            tries.get()
        };

        let leading = async {
            leadership.lead(runner, cancel.clone()).await;
        };
        let watching = async {
            tokio::time::sleep(Duration::from_millis(1)).await;
            let first = tries();
            // The first pause is at most twice the shortest.
            tokio::time::sleep(RETRY_FIRST * 2 + Duration::from_secs(1)).await;
            let second = tries();
            cancel.cancel();
            (first, second)
        };
        let ((), (first, second)) = tokio::join!(leading, watching);

        assert_eq!(first, 1, "it is tried at once");
        assert!(second >= 2, "and again after the pause: {second}");
        assert!(job.store.expired.lock().unwrap().is_empty(), "no job ran");
    }

    /// A leader election whose watch the test drives.
    #[derive(Default)]
    struct DrivenElection {
        sender: std::sync::Mutex<Option<cluster_sdk::LeaderWatchSender>>,
    }

    impl DrivenElection {
        fn joined(&self) -> bool {
            self.sender.lock().unwrap().is_some()
        }
    }

    #[async_trait]
    impl cluster_sdk::LeaderElectionBackend for DrivenElection {
        fn features(&self) -> cluster_sdk::LeaderElectionFeatures {
            cluster_sdk::LeaderElectionFeatures::new(true)
        }

        async fn elect(
            &self,
            _name: &str,
        ) -> Result<cluster_sdk::LeaderWatch, cluster_sdk::ClusterError> {
            let (sender, _resign, watch) =
                cluster_sdk::LeaderWatch::channel(8, cluster_sdk::LeaderStatus::Follower);
            *self.sender.lock().unwrap() = Some(sender);
            Ok(watch)
        }

        async fn elect_with_config(
            &self,
            name: &str,
            _config: cluster_sdk::ElectionConfig,
        ) -> Result<cluster_sdk::LeaderWatch, cluster_sdk::ClusterError> {
            self.elect(name).await
        }
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn the_job_runs_while_this_instance_leads_and_stops_when_it_loses_the_lead() {
        let job = job();
        job.store
            .mark_at(job.key, OffsetDateTime::now_utc() - ERASURE_WAIT * 2);
        let runner = Arc::new(DeletionJob {
            deletion: Arc::clone(&job.deletion),
            store: Arc::clone(&job.store) as _,
            periods: periods(RetentionConfig::default()),
            interval: Duration::from_secs(60),
            retention_interval: Duration::from_secs(60),
            exit: crate::test_support::tenant_exit_without_resolver(Arc::clone(&job.store)),
        });
        let election = Arc::new(DrivenElection::default());
        let hub = ClientHub::new();
        cluster_sdk::test_support::StubClusterClient::for_profile("construct")
            .with_leader_election(Arc::clone(&election) as _)
            .register(&hub);
        let leadership = ClusterLeadership::new(Arc::new(hub));
        let cancel = CancellationToken::new();
        let leading = tokio::spawn({
            let cancel = cancel.clone();
            async move { leadership.lead(runner, cancel).await }
        });

        until("the election is joined", || election.joined()).await;
        assert!(job.store.is_marked(job.key), "a follower runs nothing");
        let sender = election.sender.lock().unwrap().take().unwrap();
        sender
            .send_status(cluster_sdk::LeaderStatus::Leader)
            .await
            .unwrap();
        until("the leader runs the erasure", || {
            job.store.finished() == [job.key]
        })
        .await;
        sender
            .send_status(cluster_sdk::LeaderStatus::Lost)
            .await
            .unwrap();
        until("the job stops with the lead", || {
            logs_contain("this instance no longer runs the deletion job")
        })
        .await;

        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(5), leading)
            .await
            .expect("the leadership ends on cancel")
            .unwrap();
        assert!(logs_contain("this instance leads the deletion job"));
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn an_erasure_that_keeps_failing_goes_behind_the_others_and_says_so() {
        let job = job();
        let (failing, done) = (job.key, another_subject());
        let started = OffsetDateTime::now_utc();
        job.store.mark_at(failing, started - ERASURE_WAIT * 3);
        job.store.mark_at(done, started - ERASURE_WAIT * 2);
        job.roots
            .answers
            .lock()
            .unwrap()
            .push_back(Err(DomainError::internal("a foreign root")));
        let runner = DeletionJob {
            deletion: Arc::clone(&job.deletion),
            store: Arc::clone(&job.store) as _,
            periods: periods(RetentionConfig::default()),
            interval: Duration::from_secs(60),
            retention_interval: Duration::from_secs(60),
            exit: crate::test_support::tenant_exit_without_resolver(Arc::clone(&job.store)),
        };

        runner.run_once().await;

        assert_eq!(job.store.finished(), [done], "the other one still ran");
        let attempted = job.store.attempts.lock().unwrap().clone();
        assert!(
            attempted
                .get(&(failing.tenant_id, failing.subject_id))
                .is_some_and(|at| *at >= started),
            "{attempted:?}"
        );
        assert!(logs_contain("a rerun does not cure"));

        // One marked after the failing one, but never tried, now runs first:
        // it takes the next failure, and the one tried before gets through.
        let later = another_subject();
        job.store.mark_at(later, started - ERASURE_WAIT * 2);
        job.roots
            .answers
            .lock()
            .unwrap()
            .push_back(Err(DomainError::internal("a foreign root")));

        runner.run_once().await;

        assert_eq!(job.store.finished(), [done, failing]);
        assert!(job.store.is_marked(later));
    }

    // --- tenant exit --------------------------------------------------------

    use crate::domain::tenant_exit::{GraphExit, TenantStatus};
    use crate::test_support::{ScriptedOffboarding, ScriptedStatuses, tenant_exit_over};

    /// The job over `job`'s stores with `retention`, and tenant exit over
    /// `statuses` and `graph`, as a processing identity.
    fn exiting(
        job: &Job,
        retention: RetentionConfig,
        statuses: &Arc<ScriptedStatuses>,
        graph: &Arc<ScriptedOffboarding>,
    ) -> DeletionJob {
        DeletionJob {
            exit: Arc::new(tenant_exit_over(
                Some(Uuid::new_v4()),
                Arc::clone(&job.store),
                Arc::clone(statuses) as _,
                Arc::clone(graph),
                Arc::default(),
            )),
            ..runner(job, retention)
        }
    }

    fn every_tenant(days: u32) -> RetentionConfig {
        RetentionConfig {
            default_days: Some(NonZeroU32::new(days).unwrap()),
            tenants: std::collections::BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn a_deleted_tenant_exits_and_is_not_swept_while_another_is_swept_as_before() {
        let job = job();
        let deleted = Uuid::new_v4();
        job.store
            .tenants
            .lock()
            .unwrap()
            .extend([job.key.tenant_id, deleted]);
        let statuses = Arc::new(ScriptedStatuses::default());
        statuses.set(deleted, TenantStatus::Deleted);
        let graph = Arc::new(ScriptedOffboarding::default());

        exiting(&job, every_tenant(30), &statuses, &graph)
            .run_once()
            .await;

        assert_eq!(job.store.emptied(), [deleted]);
        assert_eq!(graph.asked(), [(deleted, deleted)]);
        assert_eq!(job.store.forgotten(), [deleted]);
        assert_eq!(
            swept(&job),
            [job.key.tenant_id],
            "only the present tenant is swept"
        );
    }

    #[tokio::test]
    async fn a_deleted_tenant_exits_also_where_no_retention_period_is_set() {
        let job = job();
        let deleted = Uuid::new_v4();
        job.store.tenants.lock().unwrap().insert(deleted);
        let statuses = Arc::new(ScriptedStatuses::default());
        statuses.set(deleted, TenantStatus::Deleted);
        let graph = Arc::new(ScriptedOffboarding::default());

        exiting(&job, RetentionConfig::default(), &statuses, &graph)
            .run_once()
            .await;

        assert_eq!(job.store.forgotten(), [deleted]);
        assert!(swept(&job).is_empty());
    }

    #[tokio::test]
    async fn a_tenant_that_is_present_or_unknown_is_left_untouched() {
        let job = job();
        let unknown = Uuid::new_v4();
        job.store
            .tenants
            .lock()
            .unwrap()
            .extend([job.key.tenant_id, unknown]);
        let statuses = Arc::new(ScriptedStatuses::default());
        statuses.set(unknown, TenantStatus::Unknown);
        let graph = Arc::new(ScriptedOffboarding::default());

        exiting(&job, RetentionConfig::default(), &statuses, &graph)
            .run_once()
            .await;

        let asked: BTreeSet<Uuid> = statuses.asked().into_iter().map(|(t, _)| t).collect();
        assert_eq!(asked, BTreeSet::from([job.key.tenant_id, unknown]));
        assert!(job.store.emptied().is_empty());
        assert!(graph.asked().is_empty());
        assert_eq!(job.profile.live().len(), 1, "nothing is removed");
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn a_tenant_whose_status_cannot_be_read_is_swept_as_before_and_asked_again() {
        let job = job();
        job.store.tenants.lock().unwrap().insert(job.key.tenant_id);
        let statuses = Arc::new(ScriptedStatuses::default());
        statuses.fail(job.key.tenant_id);
        let graph = Arc::new(ScriptedOffboarding::default());
        let runner = exiting(&job, every_tenant(30), &statuses, &graph);

        runner.run_once().await;
        assert_eq!(swept(&job), [job.key.tenant_id]);
        assert!(job.store.emptied().is_empty());
        assert!(logs_contain(
            "whether the tenant is deleted could not be read"
        ));

        statuses.down.lock().unwrap().clear();
        statuses.set(job.key.tenant_id, TenantStatus::Deleted);
        runner.run_once().await;
        assert_eq!(job.store.forgotten(), [job.key.tenant_id]);
    }

    #[tokio::test]
    async fn an_exit_that_graph_storage_has_not_finished_is_finished_by_a_later_run() {
        let job = job();
        let deleted = job.key.tenant_id;
        job.store.tenants.lock().unwrap().insert(deleted);
        let statuses = Arc::new(ScriptedStatuses::default());
        statuses.set(deleted, TenantStatus::Deleted);
        let graph = Arc::new(ScriptedOffboarding::default());
        graph.answer(Ok(GraphExit::UnderWay));
        graph.answer(Err(DomainError::Unavailable(
            "graph storage is down".to_owned(),
        )));
        let runner = exiting(&job, every_tenant(30), &statuses, &graph);

        runner.run_once().await;
        runner.run_once().await;
        assert!(job.store.forgotten().is_empty());
        assert!(job.store.keeps(deleted));
        assert!(swept(&job).is_empty(), "a deleted tenant is never swept");

        runner.run_once().await;
        assert_eq!(job.store.forgotten(), [deleted]);
        assert_eq!(
            job.store.emptied().len(),
            2,
            "the rows go on each run graph storage accepted, not on the one it did not answer"
        );

        runner.run_once().await;
        assert_eq!(
            graph.asked().len(),
            3,
            "a forgotten tenant is not checked again"
        );
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn without_a_tenant_resolver_every_tenant_is_left_to_retention() {
        let job = job();
        job.store.tenants.lock().unwrap().insert(job.key.tenant_id);
        let runner = DeletionJob {
            exit: Arc::new(tenant_exit_over(
                Some(Uuid::new_v4()),
                Arc::clone(&job.store),
                Arc::new(crate::test_support::NoTenantResolver),
                Arc::default(),
                Arc::default(),
            )),
            ..runner(&job, every_tenant(30))
        };

        runner.run_once().await;

        assert_eq!(swept(&job), [job.key.tenant_id]);
        assert!(job.store.emptied().is_empty());
        assert!(
            !logs_contain("could not be read"),
            "said once at start, not per tenant"
        );
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn a_failed_exit_is_logged_and_the_next_tenant_still_exits() {
        let job = job();
        let (refused, next) = (Uuid::new_v4(), Uuid::new_v4());
        // The tenants are taken in order, so the refused one comes first.
        let (refused, next) = if refused < next {
            (refused, next)
        } else {
            (next, refused)
        };
        job.store.tenants.lock().unwrap().extend([refused, next]);
        let statuses = Arc::new(ScriptedStatuses::default());
        statuses.set(refused, TenantStatus::Deleted);
        statuses.set(next, TenantStatus::Deleted);
        let graph = Arc::new(ScriptedOffboarding::default());
        graph.answer(Err(DomainError::internal(
            "graph storage refused Construct's offboarding of the tenant",
        )));

        exiting(&job, RetentionConfig::default(), &statuses, &graph)
            .run_once()
            .await;

        assert_eq!(job.store.forgotten(), [next]);
        assert!(logs_contain(
            "tenant exit failed in a way a rerun does not cure"
        ));
    }

    /// Two tenants, in the order the job takes them.
    fn two_tenants() -> (Uuid, Uuid) {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        if a < b { (a, b) } else { (b, a) }
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn a_tenant_whose_exit_panics_is_logged_and_the_next_tenant_still_exits() {
        let job = job();
        let (first, next) = two_tenants();
        job.store.keep([first, next]);
        let statuses = Arc::new(ScriptedStatuses::default());
        statuses.set(first, TenantStatus::Deleted);
        statuses.set(next, TenantStatus::Deleted);
        let graph = Arc::new(ScriptedOffboarding::default());
        *graph.panic_for.lock().unwrap() = Some(first);

        exiting(&job, RetentionConfig::default(), &statuses, &graph)
            .run_once()
            .await;

        assert_eq!(job.store.forgotten(), [next]);
        assert!(job.store.keeps(first), "the next run tries it again");
        assert!(logs_contain("tenant exit panicked for the tenant"));
        assert!(logs_contain(&first.to_string()));
        assert!(
            !logs_contain("a run of the deletion job panicked"),
            "caught for the tenant, not for the run"
        );
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn a_status_check_that_panics_leaves_the_tenant_to_retention_and_goes_on() {
        let job = job();
        let next = Uuid::new_v4();
        job.store.keep([job.key.tenant_id, next]);
        let statuses = Arc::new(ScriptedStatuses::default());
        *statuses.panic_for.lock().unwrap() = Some(job.key.tenant_id);
        let graph = Arc::new(ScriptedOffboarding::default());

        exiting(&job, every_tenant(30), &statuses, &graph)
            .run_once()
            .await;

        let swept: BTreeSet<Uuid> = swept(&job).into_iter().collect();
        assert_eq!(swept, BTreeSet::from([job.key.tenant_id, next]));
        assert!(logs_contain("tenant exit panicked for the tenant"));
        assert!(job.store.emptied().is_empty());
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn an_exit_under_way_is_said_and_a_graph_storage_that_does_not_answer_is_a_warning() {
        let job = job();
        job.store.keep([job.key.tenant_id]);
        let statuses = Arc::new(ScriptedStatuses::default());
        statuses.set(job.key.tenant_id, TenantStatus::Deleted);
        let graph = Arc::new(ScriptedOffboarding::default());
        graph.answer(Ok(GraphExit::UnderWay));
        graph.answer(Err(DomainError::Unavailable(
            "graph storage is down".to_owned(),
        )));
        let runner = exiting(&job, RetentionConfig::default(), &statuses, &graph);

        runner.run_once().await;
        assert!(logs_contain(
            "graph storage is removing the deleted tenant's graph and its rows here are \
             removed; the next run asks again"
        ));

        runner.run_once().await;
        assert!(logs_contain(
            "tenant exit stopped for the tenant; the next run goes on"
        ));
        assert!(
            !logs_contain("does not cure"),
            "a graph storage that does not answer is no defect"
        );
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn a_tenant_the_resolver_does_not_know_is_kept_and_said_to_need_an_operator() {
        let job = job();
        job.store.keep([job.key.tenant_id]);
        let statuses = Arc::new(ScriptedStatuses::default());
        statuses.set(job.key.tenant_id, TenantStatus::Unknown);
        let graph = Arc::new(ScriptedOffboarding::default());

        exiting(&job, RetentionConfig::default(), &statuses, &graph)
            .run_once()
            .await;

        assert!(job.store.emptied().is_empty());
        assert!(logs_contain(
            "the tenant resolver does not know the tenant, so nothing says it was deleted; \
             its data stays until an operator removes it"
        ));
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn a_resolver_failure_a_rerun_does_not_cure_is_an_error() {
        let job = job();
        job.store.keep([job.key.tenant_id]);
        let statuses = Arc::new(ScriptedStatuses::default());
        statuses.refused.lock().unwrap().insert(job.key.tenant_id);
        let graph = Arc::new(ScriptedOffboarding::default());

        exiting(&job, RetentionConfig::default(), &statuses, &graph)
            .run_once()
            .await;

        assert!(logs_contain(
            "whether the tenant is deleted could not be read, in a way a rerun does not cure"
        ));
        assert!(!logs_contain("the next run asks again"));
    }
}
