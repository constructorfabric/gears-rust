//! The Rating gear: wiring, migrations, the REST plane and the `stateful` lifecycle (feature 01
//! §5.1).
//!
//! `init` validates the configuration, registers the authz labels in the types registry and builds
//! the services. `serve` starts the outbox work queues and the leader-only jobs (the exception
//! sweeper and one usage feed reader per source) and stops both on shutdown.
//!
//! The namespace is `bss_rating` (T-D-60, database ADR-0001). The `db_namespace` attribute the
//! ADR asks for is not implemented by the toolkit macros yet, so the namespace is applied through
//! the explicit `bss_rating__*` table names, as DESIGN §1.3 states.

// @cpt-dod:cpt-cf-bss-rating-dod-foundation-module:p1
// @cpt-dod:cpt-cf-bss-rating-dod-foundation-inbox:p1
// @cpt-dod:cpt-cf-bss-rating-dod-intake-capture:p1

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::Context as _;
use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use axum::Router;
use sea_orm_migration::{MigrationTrait, MigratorTrait};
use tenant_resolver_sdk::TenantResolverClient;
use tokio::task::{JoinError, JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
use toolkit::client_hub::{ClientHub, ClientHubError};
use toolkit::config::ConfigError;
use toolkit::contracts::{DatabaseCapability, RestApiCapability};
use toolkit::{Gear, GearCtx};
use toolkit_db::Db;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use crate::api::rest::{self, RestState};
use crate::authz::ServiceGrant;
use crate::config::{MAX_LEADER_STOP_SECS, RatingConfig, UsageFeedSettings};
use crate::domain::audit::OperatorAudit;
use crate::domain::backoff::{Backoff, jittered};
use crate::domain::error::{Cause, DomainError};
use crate::domain::exception::{ExceptionRegister, ExceptionSweeper, SweepReport};
use crate::domain::inbox::{Inbox, InboxParts};
use crate::domain::leadership::{LeaderWork, LeadershipName, LeaseProvider};
use crate::domain::ports::{Clock, Dependency, RatingMetricsPort, SweepOutcome, TenantGrant};
use crate::domain::usage::NoAttribution;
use crate::domain::usage::declarations::{DeclarationCache, MeterDeclarations};
use crate::domain::usage::feed::UsageFeedPort;
use crate::domain::usage::intake::{IntakeParts, TargetUnknownRetrier, UsageIntake};
use crate::domain::usage::metrics::UsageMetricsPort;
use crate::domain::usage::reader::{FeedGrant, ReaderParts, RunEnd, UsageFeedReader};
use crate::domain::usage::source::{Checkpoint, SourceId};
use crate::infra::clock::WallClock;
use crate::infra::cluster::{
    ClusterLeaseProvider, ElectionTiming, MAX_RESIGN_WAIT, resolve_election,
};
use crate::infra::feed_grant::PdpFeedGrant;
use crate::infra::metrics::OtelRatingMetrics;
use crate::infra::queue_depth::OutboxQueueDepth;
use crate::infra::queues::{self, OUTBOX_TABLE_PREFIX, QueueLayout, UnwiredQueue};
use crate::infra::storage::migrations::Migrator;
use crate::infra::storage::repo::audit_repo::OrmAuditRepo;
use crate::infra::storage::repo::exception_repo::OrmExceptionRepo;
use crate::infra::storage::repo::inbox_repo::OrmInboxRepo;
use crate::infra::storage::repo::usage_repo::OrmUsageRepo;
use crate::infra::tenant_hierarchy::ResolverTenantHierarchy;

/// What `init` builds and `serve` and `register_rest` use.
struct RatingRuntime {
    config: RatingConfig,
    db: Db,
    hub: Arc<ClientHub>,
    inbox: Inbox,
    sweeper: ExceptionSweeper,
    metrics: Arc<dyn UsageMetricsPort>,
    queues: QueueLayout,
    /// `None` when leadership is disabled.
    leader_timing: Option<ElectionTiming>,
    /// Usage capture and normalization (feature 04).
    usage: UsageRuntime,
}

/// What the usage feed readers are built from; the feed, the meter declarations and the tenant
/// resolver are taken from `ClientHub` when the readers start, after every gear's `init`.
struct UsageRuntime {
    settings: UsageFeedSettings,
    intake: UsageIntake,
    feed_grant: Arc<dyn FeedGrant>,
    grant: Arc<dyn TenantGrant>,
    clock: Arc<dyn Clock>,
}

/// The gear's stop budget: the host allows it before it aborts `serve`, inside its own 35 s
/// shutdown deadline.
pub const GEAR_STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// The `bss-rating` gear.
#[toolkit::gear(
    name = "bss-rating",
    capabilities = [db, rest, stateful],
    deps = [types_registry, authz_resolver],
    lifecycle(entry = "serve", stop_timeout_fn = "stop_timeout")
)]
#[derive(Default)]
pub struct BssRatingGear {
    runtime: OnceLock<Arc<RatingRuntime>>,
    /// The metrics port a test reads back; the global meter otherwise.
    #[cfg(test)]
    metrics: Option<Arc<dyn UsageMetricsPort>>,
}

impl std::fmt::Debug for BssRatingGear {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BssRatingGear")
            .field("initialised", &self.runtime.get().is_some())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl Gear for BssRatingGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let config = match ctx.config::<RatingConfig>() {
            Ok(config) => config,
            Err(ConfigError::MissingConfigSection { .. }) => RatingConfig::default(),
            // Compiled into the binary but not configured: the gear stays inactive.
            Err(ConfigError::GearNotFound { .. }) => return Ok(()),
            Err(error) => return Err(error).context("bss-rating: invalid config"),
        };
        // What is checked is what runs: the gear is built from the resolved configuration.
        let resolved = config
            .resolve()
            .context("bss-rating: configuration rejected")?;
        let leader_timing = resolved
            .election
            .map(|election| ElectionTiming::new(election, config.leadership_stop_timeout()));
        if leader_timing.is_none() {
            tracing::warn!(
                target: "bss_rating.leadership",
                "leadership is disabled: the exception sweeper does not run on this instance"
            );
        }
        let db = ctx
            .db_required()
            .context("bss-rating: database is required")?
            .db();
        let hub = ctx.client_hub();
        let authz = hub.get::<dyn AuthZResolverApi>().context(
            "bss-rating: AuthZResolverApi absent from ClientHub; the authz-resolver gear must be registered",
        )?;
        let enforcer = PolicyEnforcer::new(authz);
        register_authz_labels(&hub).await?;

        let clock: Arc<dyn Clock> = Arc::new(WallClock);
        let metrics = self.metrics_port();
        let grant: Arc<dyn TenantGrant> = Arc::new(ServiceGrant::new(enforcer.clone()));
        // No pipeline feature is built yet: no inbox source has an applier and no exception kind
        // a retrier. Each feature registers its own here as it lands.
        let inbox = Inbox::new(InboxParts {
            db: db.clone(),
            enforcer: enforcer.clone(),
            repo: Arc::new(OrmInboxRepo),
            appliers: BTreeMap::new(),
            producers: resolved.producers,
            grant: Arc::clone(&grant),
            exceptions: ExceptionRegister::new(Arc::new(OrmExceptionRepo), Arc::clone(&clock)),
            audit: OperatorAudit::new(Arc::new(OrmAuditRepo), Arc::clone(&clock)),
            metrics: Arc::clone(&metrics) as Arc<dyn RatingMetricsPort>,
            clock: Arc::clone(&clock),
        });
        let intake = UsageIntake::new(IntakeParts {
            checkpoints: Arc::new(OrmUsageRepo),
            records: Arc::new(OrmUsageRepo),
            losses: Arc::new(OrmUsageRepo),
            exceptions: ExceptionRegister::new(Arc::new(OrmExceptionRepo), Arc::clone(&clock)),
            contributions: Arc::new(NoAttribution),
            clock: Arc::clone(&clock),
            retention_floor: resolved.usage.retention_floor,
        });
        let sweeper = ExceptionSweeper::new(
            vec![Arc::new(TargetUnknownRetrier::new(intake.clone()))],
            Arc::clone(&grant),
            Arc::new(OrmExceptionRepo),
        );
        let usage = UsageRuntime {
            settings: resolved.usage,
            intake,
            feed_grant: Arc::new(PdpFeedGrant::new(enforcer)),
            grant,
            clock,
        };
        self.runtime
            .set(Arc::new(RatingRuntime {
                config,
                db,
                hub,
                inbox,
                sweeper,
                metrics,
                queues: resolved.queues,
                leader_timing,
                usage,
            }))
            .map_err(|_| anyhow::anyhow!("bss-rating: initialised twice"))?;
        tracing::info!("bss-rating initialised");
        Ok(())
    }
}

/// Registers the stub type schemas of the authz labels; a refused registration fails the boot,
/// since without them no role can grant a Rating permission.
async fn register_authz_labels(hub: &ClientHub) -> anyhow::Result<()> {
    let registry = hub.get::<dyn TypesRegistryClient>().context(
        "bss-rating: TypesRegistryClient absent from ClientHub; the types-registry gear must be registered",
    )?;
    let results = registry
        .register(crate::authz::authz_label_type_schemas())
        .await
        .context("bss-rating: register authz label schemas")?;
    for result in results {
        if let RegisterResult::Err { gts_id, error } = result {
            anyhow::bail!(
                "bss-rating: failed to register authz label {}: {error}",
                gts_id.as_deref().unwrap_or("?")
            );
        }
    }
    Ok(())
}

impl BssRatingGear {
    /// The lifecycle's stop budget, [`GEAR_STOP_TIMEOUT`].
    #[expect(
        clippy::unused_self,
        reason = "the lifecycle macro takes the stop budget from a method"
    )]
    fn stop_timeout(&self) -> Duration {
        GEAR_STOP_TIMEOUT
    }

    /// The metrics port of the gear: the global meter's instruments.
    #[cfg(not(test))]
    #[expect(
        clippy::unused_self,
        reason = "a test build reads the port a test set on the gear"
    )]
    fn metrics_port(&self) -> Arc<dyn UsageMetricsPort> {
        Arc::new(OtelRatingMetrics::from_global())
    }

    /// The metrics port a test set on the gear, or the global meter's instruments.
    #[cfg(test)]
    fn metrics_port(&self) -> Arc<dyn UsageMetricsPort> {
        self.metrics
            .clone()
            .unwrap_or_else(|| Arc::new(OtelRatingMetrics::from_global()))
    }

    /// The lifecycle entry: starts the outbox work queues and the leader-only jobs, and stops
    /// them when the gear stops.
    pub(crate) async fn serve(self: Arc<Self>, cancel: CancellationToken) -> anyhow::Result<()> {
        let Some(runtime) = self.runtime.get().cloned() else {
            cancel.cancelled().await;
            return Ok(());
        };
        let outbox = queues::start(runtime.db.clone(), &runtime.queues, |queue| {
            Arc::new(UnwiredQueue::new(queue))
        })
        .await
        .context("bss-rating: the outbox work queues failed to start")?;
        let leaders = AbortOnDrop(tokio::spawn(run_leader_jobs(
            Arc::clone(&runtime),
            cancel.child_token(),
        )));
        cancel.cancelled().await;
        // The leader jobs stop and resign while the outbox stops, together inside the gear's
        // stop budget.
        let ((), ended) = tokio::join!(outbox.stop(), leaders.join_within(LEADER_JOIN_BUDGET));
        ended.context("bss-rating: the leader jobs stopped unexpectedly")
    }
}

/// How long `serve` waits for the leader jobs to stop and resign: the leader stop budget (at most
/// [`MAX_LEADER_STOP_SECS`]) and the resign budget ([`MAX_RESIGN_WAIT`]).
const LEADER_JOIN_BUDGET: Duration =
    Duration::from_secs(MAX_LEADER_STOP_SECS + MAX_RESIGN_WAIT.as_secs());

// Raising either budget past the gear's own fails the build, not a shutdown.
const _: () = assert!(LEADER_JOIN_BUDGET.as_secs() < GEAR_STOP_TIMEOUT.as_secs());

/// The pause before the first retry of the leader election.
const FIRST_RETRY: Duration = Duration::from_secs(1);

/// A task that is aborted when its handle is dropped, so a `serve` the host aborts leaves no
/// leader job running.
struct AbortOnDrop(JoinHandle<()>);

impl AbortOnDrop {
    /// Waits for the task up to `budget`, then aborts it.
    async fn join_within(mut self, budget: Duration) -> Result<(), JoinError> {
        if let Ok(joined) = tokio::time::timeout(budget, &mut self.0).await {
            return joined;
        }
        tracing::warn!(target: "bss_rating.leadership", ?budget, "the leader jobs did not stop within the budget; aborted");
        self.0.abort();
        Ok(())
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Resolves the Cluster backend (retrying with backoff while it is not bound, R-27) and runs the
/// exception sweeper whenever this instance holds the `exception-sweep` leadership, and each usage
/// source's reader whenever it holds that source's `usage-feed-{uuid}` leadership.
async fn run_leader_jobs(runtime: Arc<RatingRuntime>, stop: CancellationToken) {
    let Some(timing) = runtime.leader_timing else {
        return;
    };
    let cap = runtime.config.leadership_resolve_retry_max();
    let sweep = retry_with_backoff(&stop, cap, runtime.metrics.as_ref(), || {
        lead_exception_sweep(&runtime, timing, &stop)
    });
    tokio::join!(sweep, run_usage_readers(&runtime, timing, &stop));
}

/// Reconciles the stored usage sources with the configuration once, then runs a reader for every
/// source that is not retired, each under its own leadership (feature 04 §2.1 `inst-rp-lock`). A
/// reader task that fails is logged and started again after a growing pause.
async fn run_usage_readers(
    runtime: &Arc<RatingRuntime>,
    timing: ElectionTiming,
    stop: &CancellationToken,
) {
    let cap = runtime.config.leadership_resolve_retry_max();
    let metrics = runtime.metrics.as_ref();
    let Some(sources) = until_ok(stop, cap, metrics, || reconcile_sources(runtime)).await else {
        return;
    };
    if sources.is_empty() {
        return;
    }
    // Every gear's `init` has run by now, so a client not bound now is never bound.
    let reader = match usage_reader(runtime) {
        Ok(reader) => reader,
        Err(error) => {
            if let Some((dependency, code)) = error.dependency_failure() {
                metrics.dependency_error(dependency, code);
            }
            tracing::error!(
                target: "bss_rating.usage",
                %error,
                sources = sources.len(),
                "the usage feed readers cannot start; no usage is read on this instance"
            );
            return;
        }
    };
    let mut readers = ReaderTasks {
        runtime: Arc::clone(runtime),
        reader,
        timing,
        stop: stop.clone(),
        tasks: JoinSet::new(),
        sources: HashMap::new(),
        restarts: HashMap::new(),
    };
    for checkpoint in sources {
        readers.spawn(checkpoint.source_id, Duration::ZERO);
    }
    readers.supervise().await;
}

/// The reader tasks of one instance, one per source, each started again after a growing pause if
/// it fails.
struct ReaderTasks {
    runtime: Arc<RatingRuntime>,
    reader: UsageFeedReader,
    timing: ElectionTiming,
    stop: CancellationToken,
    tasks: JoinSet<()>,
    sources: HashMap<tokio::task::Id, SourceId>,
    restarts: HashMap<SourceId, Backoff>,
}

impl ReaderTasks {
    fn spawn(&mut self, source: SourceId, delay: Duration) {
        let task = self.tasks.spawn(read_source(
            Arc::clone(&self.runtime),
            self.reader.clone(),
            self.timing,
            self.stop.clone(),
            source,
            delay,
        ));
        self.sources.insert(task.id(), source);
    }

    /// Waits for the tasks to end, which they do when the gear stops; one that fails before is
    /// logged and started again.
    async fn supervise(&mut self) {
        while let Some(joined) = self.tasks.join_next_with_id().await {
            let (id, failure) = match joined {
                Ok((id, ())) => (id, None),
                Err(error) => (error.id(), Some(error)),
            };
            let source = self.sources.remove(&id);
            if let (Some(source), Some(failure)) = (source, failure)
                && !self.stop.is_cancelled()
            {
                self.restart(source, &failure);
            }
        }
    }

    fn restart(&mut self, source: SourceId, failure: &JoinError) {
        let cap = self.runtime.config.leadership_resolve_retry_max();
        let pause = self
            .restarts
            .entry(source)
            .or_insert_with(|| Backoff::new(cap))
            .pause();
        tracing::error!(
            target: "bss_rating.usage",
            source = %source,
            error = %failure,
            retry_in = ?pause,
            "a usage feed reader task failed; starting it again"
        );
        self.spawn(source, pause);
    }
}

/// Leads `source` while it is not retired, after `delay`: reads it during every term this
/// instance wins, gives the leadership up once the source is retired (holding it idle would keep
/// every other instance from reading the source if a reconciliation made it active again), and
/// campaigns again once the source is readable.
async fn read_source(
    runtime: Arc<RatingRuntime>,
    reader: UsageFeedReader,
    timing: ElectionTiming,
    stop: CancellationToken,
    source: SourceId,
    delay: Duration,
) {
    if !sleep_unless_stopped(&stop, delay).await {
        return;
    }
    let cap = runtime.config.leadership_resolve_retry_max();
    let recheck = reader.settings().poll_interval();
    loop {
        let retired = stop.child_token();
        retry_with_backoff(&retired, cap, runtime.metrics.as_ref(), || {
            lead_usage_feed(&runtime, timing, &retired, &reader, source)
        })
        .await;
        if stop.is_cancelled() || !until_readable(&reader, source, recheck, &stop).await {
            return;
        }
    }
}

/// Sleeps `delay`; `false` when `stop` fires first.
async fn sleep_unless_stopped(stop: &CancellationToken, delay: Duration) -> bool {
    tokio::select! {
        () = stop.cancelled() => false,
        () = tokio::time::sleep(delay) => true,
    }
}

/// Checks every `recheck` whether the retired `source` is readable again; `false` when `stop`
/// fires first.
async fn until_readable(
    reader: &UsageFeedReader,
    source: SourceId,
    recheck: Duration,
    stop: &CancellationToken,
) -> bool {
    tracing::info!(target: "bss_rating.usage", source = %source, "usage source retired; leadership given up");
    while sleep_unless_stopped(stop, recheck).await {
        if readable_now(reader, source).await {
            return true;
        }
    }
    false
}

/// Whether the retired `source` is readable again; a failed read is logged and counts as no.
async fn readable_now(reader: &UsageFeedReader, source: SourceId) -> bool {
    match reader.is_readable(source).await {
        Ok(readable) => {
            if readable {
                tracing::info!(target: "bss_rating.usage", source = %source, "usage source active again; leading it");
            }
            readable
        }
        Err(error) => {
            tracing::warn!(target: "bss_rating.usage", source = %source, %error, "could not read whether a retired usage source is active again");
            false
        }
    }
}

/// Runs `attempt` until it succeeds or `stop` fires, pausing between failures as
/// [`retry_with_backoff`] does; `None` when stopped first.
pub(crate) async fn until_ok<T, F, Fut>(
    stop: &CancellationToken,
    cap: Duration,
    metrics: &dyn RatingMetricsPort,
    mut attempt: F,
) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, DomainError>>,
{
    let mut backoff = FIRST_RETRY.min(cap);
    while !stop.is_cancelled() {
        let error = match attempt().await {
            Ok(value) => return Some(value),
            Err(error) => error,
        };
        if stop.is_cancelled() {
            return None;
        }
        let pause = jittered(backoff);
        report_attempt(&Err(error), pause, metrics);
        tokio::select! {
            () = stop.cancelled() => return None,
            () = tokio::time::sleep(pause) => {}
        }
        backoff = (backoff * 2).min(cap);
    }
    None
}

/// The stored sources reconciled with the configuration: the ones to read.
async fn reconcile_sources(runtime: &RatingRuntime) -> Result<Vec<Checkpoint>, DomainError> {
    let usage = &runtime.usage;
    let mut owner_scopes = BTreeMap::new();
    for source in &usage.settings.sources {
        let owner = source.tenant_scope().owner();
        if let std::collections::btree_map::Entry::Vacant(slot) = owner_scopes.entry(owner) {
            slot.insert(usage.grant.scope_for(owner).await?);
        }
    }
    usage
        .intake
        .reconcile_sources(
            &runtime.db,
            usage.settings.sources.clone(),
            usage.settings.config_generation,
            owner_scopes,
        )
        .await
}

/// The client of type `T` bound in `ClientHub`: a missing one is `dependency` unavailable, one of
/// another type a wiring fault.
fn bound<T>(hub: &ClientHub, dependency: Dependency, what: &str) -> Result<Arc<T>, DomainError>
where
    T: ?Sized + Send + Sync + 'static,
{
    hub.get::<T>().map_err(|error| match error {
        ClientHubError::NotFound { .. } | ClientHubError::ScopedNotFound { .. } => {
            DomainError::DependencyUnavailable {
                dependency,
                detail: format!("no {what} is bound in ClientHub"),
                cause: Some(Cause::new(error)),
            }
        }
        ClientHubError::TypeMismatch { .. } | ClientHubError::ScopedTypeMismatch { .. } => {
            DomainError::Internal(
                format!("the {what} bound in ClientHub is of another type"),
                Some(Cause::new(error)),
            )
        }
    })
}

/// The reader over the clients bound in `ClientHub`. Rating's own feed port is bound only by the
/// test providers until the adapter over the usage-collector v2 SDK lands (T-D-84); until then a
/// production instance with subscriptions configured reports the feed missing once and reads
/// nothing.
fn usage_reader(runtime: &RatingRuntime) -> Result<UsageFeedReader, DomainError> {
    let usage = &runtime.usage;
    let hub = &runtime.hub;
    let feed = bound::<dyn UsageFeedPort>(hub, Dependency::UsageFeed, "usage feed")?;
    let declarations =
        bound::<dyn MeterDeclarations>(hub, Dependency::TypesRegistry, "meter declaration read")?;
    let resolver =
        bound::<dyn TenantResolverClient>(hub, Dependency::TenantHierarchy, "tenant resolver")?;
    let settings = usage.settings.reader;
    Ok(UsageFeedReader::new(ReaderParts {
        db: runtime.db.clone(),
        feed,
        declarations: DeclarationCache::new(declarations, settings.call_timeout()),
        feed_grant: Arc::clone(&usage.feed_grant),
        hierarchy: Arc::new(ResolverTenantHierarchy::new(resolver)),
        tenant_grant: Arc::clone(&usage.grant),
        depth: Arc::new(OutboxQueueDepth::new(runtime.db.clone())),
        intake: usage.intake.clone(),
        checkpoints: Arc::new(OrmUsageRepo),
        losses: Arc::new(OrmUsageRepo),
        metrics: Arc::clone(&runtime.metrics),
        clock: Arc::clone(&usage.clock),
        settings,
    }))
}

/// One attempt: resolve the election and read `source` during every term until `stop`. A term
/// that finds the source retired cancels `stop`, so the leadership is resigned and the caller's
/// retry loop ends.
async fn lead_usage_feed(
    runtime: &Arc<RatingRuntime>,
    timing: ElectionTiming,
    stop: &CancellationToken,
    reader: &UsageFeedReader,
    source: SourceId,
) -> Result<(), DomainError> {
    let election = resolve_election(&runtime.hub).await?;
    let (reader, retired) = (reader.clone(), stop.clone());
    let work: LeaderWork = Arc::new(move |term: CancellationToken| {
        let (reader, retired) = (reader.clone(), retired.clone());
        Box::pin(async move {
            if reader.run(source, term).await == RunEnd::Retired {
                retired.cancel();
            }
        })
    });
    ClusterLeaseProvider::new(election, timing)
        .run_while_leader(LeadershipName::UsageFeed(source.uuid()), stop.clone(), work)
        .await
}

/// Runs `attempt` until `stop`, pausing after each one that ends before `stop` fires. The pause
/// doubles up to `cap`, is jittered so instances that failed together do not retry together, and
/// starts over after an attempt that held up longer than `cap`. An attempt that returns `Ok`
/// before `stop` (an election that ended cleanly, say) is paused after as a failure is, so no
/// attempt can turn the loop hot.
pub(crate) async fn retry_with_backoff<F, Fut>(
    stop: &CancellationToken,
    cap: Duration,
    metrics: &dyn RatingMetricsPort,
    mut attempt: F,
) where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(), DomainError>>,
{
    let first = FIRST_RETRY.min(cap);
    let mut backoff = first;
    while !stop.is_cancelled() {
        let started = tokio::time::Instant::now();
        let ended = attempt().await;
        if stop.is_cancelled() {
            return;
        }
        if started.elapsed() > cap {
            backoff = first;
        }
        let pause = jittered(backoff);
        report_attempt(&ended, pause, metrics);
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep(pause) => {}
        }
        backoff = (backoff * 2).min(cap);
    }
}

/// Logs how a leader attempt ended before `stop`, and counts the dependency that failed it.
fn report_attempt(
    ended: &Result<(), DomainError>,
    pause: Duration,
    metrics: &dyn RatingMetricsPort,
) {
    match ended {
        Ok(()) => {
            tracing::warn!(target: "bss_rating.leadership", retry_in = ?pause, "the leader job ended before the gear stopped; starting over");
        }
        Err(error) => {
            if let Some((dependency, code)) = error.dependency_failure() {
                metrics.dependency_error(dependency, code);
            }
            tracing::warn!(target: "bss_rating.leadership", %error, retry_in = ?pause, "the leader job ended with an error; retrying");
        }
    }
}

/// One attempt: resolve the election and run the sweeper during every term until `stop`.
async fn lead_exception_sweep(
    runtime: &Arc<RatingRuntime>,
    timing: ElectionTiming,
    stop: &CancellationToken,
) -> Result<(), DomainError> {
    let election = resolve_election(&runtime.hub).await?;
    ClusterLeaseProvider::new(election, timing)
        .run_while_leader(
            LeadershipName::ExceptionSweep,
            stop.clone(),
            sweep_work(Arc::clone(runtime)),
        )
        .await
}

/// The exception sweeper's term of leadership: a sweep every interval until the term ends.
fn sweep_work(runtime: Arc<RatingRuntime>) -> LeaderWork {
    Arc::new(move |term: CancellationToken| {
        let runtime = Arc::clone(&runtime);
        Box::pin(async move {
            sweep_every(
                &runtime.sweeper,
                &runtime.db,
                runtime.metrics.as_ref(),
                runtime.config.exception_sweep_interval(),
                &term,
            )
            .await;
        })
    })
}

/// A sweep every `interval`, the first one at once, until `term` ends; each sweep is counted and
/// timed.
pub(crate) async fn sweep_every(
    sweeper: &ExceptionSweeper,
    db: &Db,
    metrics: &dyn RatingMetricsPort,
    interval: Duration,
    term: &CancellationToken,
) {
    let mut ticks = tokio::time::interval(interval);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            () = term.cancelled() => return,
            _ = ticks.tick() => {}
        }
        let started = tokio::time::Instant::now();
        let swept = sweeper.sweep_once(db, term).await;
        record_sweep(metrics, &swept, started.elapsed(), term.is_cancelled());
    }
}

fn record_sweep(
    metrics: &dyn RatingMetricsPort,
    swept: &Result<SweepReport, DomainError>,
    duration: Duration,
    cancelled: bool,
) {
    match swept {
        Ok(report) => {
            let outcome = if cancelled {
                SweepOutcome::Cancelled
            } else {
                SweepOutcome::Completed
            };
            metrics.exception_sweep(outcome, report, duration);
            if report.retried > 0 || report.failed > 0 {
                tracing::info!(
                    target: "bss_rating.exception_sweep",
                    tenants = report.tenants,
                    retried = report.retried,
                    failed = report.failed,
                    "exception sweep"
                );
            }
        }
        Err(error) => {
            metrics.exception_sweep(SweepOutcome::Failed, &SweepReport::default(), duration);
            if let Some((dependency, code)) = error.dependency_failure() {
                metrics.dependency_error(dependency, code);
            }
            tracing::warn!(target: "bss_rating.exception_sweep", %error, "exception sweep failed");
        }
    }
}

impl DatabaseCapability for BssRatingGear {
    fn migrations(&self) -> Vec<Box<dyn MigrationTrait>> {
        let mut migrations = Migrator::migrations();
        match toolkit_db::outbox::outbox_migrations_with_prefix(OUTBOX_TABLE_PREFIX) {
            Ok(outbox) => migrations.extend(outbox),
            Err(error) => migrations.push(Box::new(InvalidOutboxMigration(error.to_string()))),
        }
        migrations
    }
}

impl RestApiCapability for BssRatingGear {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router> {
        let Some(runtime) = self.runtime.get() else {
            return Ok(router);
        };
        let state = Arc::new(RestState {
            inbox: runtime.inbox.clone(),
        });
        Ok(router.merge(rest::router(state, openapi)))
    }
}

/// `DatabaseCapability::migrations` cannot return an error: an invalid outbox prefix becomes a
/// migration that fails, never a silently missing queue table.
struct InvalidOutboxMigration(String);

impl sea_orm_migration::MigrationName for InvalidOutboxMigration {
    fn name(&self) -> &'static str {
        "invalid_bss_rating_outbox_prefix"
    }
}

#[async_trait]
impl MigrationTrait for InvalidOutboxMigration {
    async fn up(&self, _: &sea_orm_migration::SchemaManager) -> Result<(), sea_orm::DbErr> {
        Err(sea_orm::DbErr::Migration(self.0.clone()))
    }

    async fn down(&self, _: &sea_orm_migration::SchemaManager) -> Result<(), sea_orm::DbErr> {
        Err(sea_orm::DbErr::Migration(self.0.clone()))
    }
}

#[cfg(test)]
#[path = "gear_tests.rs"]
mod tests;
