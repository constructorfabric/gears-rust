//! The Rating gear: wiring, migrations, the REST plane and the `stateful` lifecycle (feature 01
//! §5.1).
//!
//! `init` validates the configuration, registers the authz labels in the types registry and builds
//! the foundation's services. `serve` starts the outbox work queues and the leader-only jobs
//! (the exception sweeper) and stops both on shutdown.
//!
//! The namespace is `bss_rating` (T-D-60, database ADR-0001). The `db_namespace` attribute the
//! ADR asks for is not implemented by the toolkit macros yet, so the namespace is applied through
//! the explicit `bss_rating__*` table names, as DESIGN §1.3 states.

// @cpt-dod:cpt-cf-bss-rating-dod-foundation-module:p1
// @cpt-dod:cpt-cf-bss-rating-dod-foundation-inbox:p1

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::Context as _;
use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use axum::Router;
use cluster_sdk::{ClusterError, ElectionConfig};
use sea_orm_migration::{MigrationTrait, MigratorTrait};
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
use toolkit::client_hub::ClientHub;
use toolkit::config::ConfigError;
use toolkit::contracts::{DatabaseCapability, RestApiCapability};
use toolkit::{Gear, GearCtx};
use toolkit_db::Db;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use crate::api::rest::{self, RestState};
use crate::authz::ServiceGrant;
use crate::config::{MAX_LEADER_STOP_SECS, RatingConfig};
use crate::domain::audit::OperatorAudit;
use crate::domain::error::DomainError;
use crate::domain::exception::{ExceptionRegister, ExceptionSweeper, SweepReport};
use crate::domain::inbox::{Inbox, InboxParts};
use crate::domain::leadership::{LeaderWork, LeadershipName, LeaseProvider};
use crate::domain::ports::{
    Clock, Dependency, FailureCode, RatingMetricsPort, SweepOutcome, TenantGrant,
};
use crate::infra::clock::WallClock;
use crate::infra::cluster::{ClusterLeaseProvider, ElectionTiming, resolve_election};
use crate::infra::metrics::OtelRatingMetrics;
use crate::infra::queues::{self, OUTBOX_TABLE_PREFIX, QueueLayout, UnwiredQueue};
use crate::infra::storage::migrations::Migrator;
use crate::infra::storage::repo::audit_repo::OrmAuditRepo;
use crate::infra::storage::repo::exception_repo::OrmExceptionRepo;
use crate::infra::storage::repo::inbox_repo::OrmInboxRepo;

/// What `init` builds and `serve` and `register_rest` use.
struct RatingRuntime {
    config: RatingConfig,
    db: Db,
    hub: Arc<ClientHub>,
    inbox: Inbox,
    sweeper: ExceptionSweeper,
    metrics: Arc<dyn RatingMetricsPort>,
    queues: QueueLayout,
    /// `None` when leadership is disabled.
    leader_timing: Option<ElectionTiming>,
}

/// The `bss-rating` gear.
#[toolkit::gear(
    name = "bss-rating",
    capabilities = [db, rest, stateful],
    deps = [types_registry, authz_resolver],
    lifecycle(entry = "serve", stop_timeout = "30s")
)]
#[derive(Default)]
pub struct BssRatingGear {
    runtime: OnceLock<Arc<RatingRuntime>>,
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
        config
            .validate()
            .context("bss-rating: configuration rejected")?;
        let queues = config.queue_layout()?;
        let producers = config.producer_policy()?;
        let leader_timing =
            election_timing(&config).context("bss-rating: invalid leadership timing")?;
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
        let metrics: Arc<dyn RatingMetricsPort> = Arc::new(OtelRatingMetrics::from_global());
        let grant: Arc<dyn TenantGrant> = Arc::new(ServiceGrant::new(enforcer.clone()));
        // No pipeline feature is built yet: no inbox source has an applier and no exception kind
        // a retrier. Each feature registers its own here as it lands.
        let inbox = Inbox::new(InboxParts {
            db: db.clone(),
            enforcer: enforcer.clone(),
            repo: Arc::new(OrmInboxRepo),
            appliers: BTreeMap::new(),
            producers,
            grant: Arc::clone(&grant),
            exceptions: ExceptionRegister::new(Arc::new(OrmExceptionRepo), Arc::clone(&clock)),
            audit: OperatorAudit::new(Arc::new(OrmAuditRepo), Arc::clone(&clock)),
            metrics: Arc::clone(&metrics),
            clock,
        });
        let sweeper = ExceptionSweeper::new(Vec::new(), grant, Arc::new(OrmExceptionRepo));
        self.runtime
            .set(Arc::new(RatingRuntime {
                config,
                db,
                hub,
                inbox,
                sweeper,
                metrics,
                queues,
                leader_timing,
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
/// [`MAX_LEADER_STOP_SECS`]) and the resign budget, inside the gear's 30 s stop budget.
const LEADER_JOIN_BUDGET: Duration = Duration::from_secs(MAX_LEADER_STOP_SECS + 5);

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
/// exception sweeper whenever this instance holds the `exception-sweep` leadership.
async fn run_leader_jobs(runtime: Arc<RatingRuntime>, stop: CancellationToken) {
    let Some(timing) = runtime.leader_timing else {
        return;
    };
    let cap = runtime.config.leadership_resolve_retry_max();
    retry_with_backoff(&stop, cap, runtime.metrics.as_ref(), || {
        lead_exception_sweep(&runtime, timing, &stop)
    })
    .await;
}

/// Runs `attempt` until `stop`, pausing after each failure. The pause doubles up to `cap`, is
/// jittered so instances that failed together do not retry together, and starts over after an
/// attempt that held up longer than `cap`.
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
        // `Ok` only when `stop` fired.
        let Err(error) = attempt().await else {
            continue;
        };
        if started.elapsed() > cap {
            backoff = first;
        }
        if matches!(error, DomainError::Coordination(..)) {
            metrics.dependency_error(Dependency::Cluster, FailureCode::LeadershipUnavailable);
        }
        let pause = jittered(backoff);
        tracing::warn!(target: "bss_rating.leadership", %error, retry_in = ?pause, "the leader job ended with an error; retrying");
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep(pause) => {}
        }
        backoff = (backoff * 2).min(cap);
    }
}

/// `backoff` scaled by a random factor in `[0.5, 1]`.
fn jittered(backoff: Duration) -> Duration {
    let mut bytes = [0_u8; 4];
    let unit = if aws_lc_rs::rand::fill(&mut bytes).is_ok() {
        f64::from(u32::from_le_bytes(bytes)) / f64::from(u32::MAX)
    } else {
        1.0
    };
    backoff.mul_f64(0.5 + 0.5 * unit)
}

/// The election timing, or `None` when leadership is disabled. A timing the Cluster SDK refuses
/// fails the boot rather than leaving an instance that looks healthy but never runs a leader job.
fn election_timing(config: &RatingConfig) -> Result<Option<ElectionTiming>, ClusterError> {
    if !config.leadership.enabled {
        tracing::warn!(
            target: "bss_rating.leadership",
            "leadership is disabled: the exception sweeper does not run on this instance"
        );
        return Ok(None);
    }
    let election = ElectionConfig::new(
        config.leadership_ttl(),
        config.leadership.max_missed_renewals,
    )?;
    Ok(Some(ElectionTiming::new(
        election,
        config.leadership_stop_timeout(),
    )))
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
            let (dependency, code) = match error {
                DomainError::AuthzUnavailable(..) => {
                    (Dependency::Pdp, FailureCode::EvaluationFailed)
                }
                _ => (Dependency::Database, FailureCode::ExceptionSweepFailed),
            };
            metrics.dependency_error(dependency, code);
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
