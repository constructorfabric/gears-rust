//! The gear scaffold and its initialization.
//!
//! `init` validates the deployment configuration fail-closed, acquires the
//! database, registers the setting-type base with the types registry, builds
//! every service, and registers `SettingsReaderClient` and
//! `SettingsContributionClient` into `ClientHub` for the gears that consume
//! them. No `@cpt-dod` marker for `dod-gear-foundation-gear-scaffold` yet:
//! that definition of done also asks for the remaining GTS control-plane
//! schemas and the category seed at init, which are still open. The marker
//! lands with the tick.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use sea_orm_migration::MigrationTrait;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
use toolkit::lifecycle::ReadySignal;
use toolkit::{DatabaseCapability, Gear, GearCtx, RestApiCapability};
use toolkit_db::{DBProvider, DbError};
use tracing::info;
use types_registry_sdk::TypesRegistryClient;

use crate::domain::platform_scope::PlatformScope;
use crate::domain::validation::TypeValidator;
use settings_service_sdk::api::{SettingsContributionClient, SettingsReaderClient};

use crate::config::SettingsServiceConfig;
use crate::log_text::LogSafe;

/// The Settings Service gear.
///
/// Holds what initialization resolves, so later phases can hang services off it
/// without changing the startup contract.
///
/// `deps` names the one gear this service calls during its **own** init — the
/// types registry — because a `deps` entry is an ordering claim that a gear
/// reading settings during *its* init could turn into an unsortable cycle
/// (DESIGN.md §4.9). Everything else is consumed: the authorization resolver is
/// declared below and wired by the runtime's proxy-wiring phase after init, so
/// the enforcer fetches it from the hub on first use; the tenant resolver is
/// fetched the same way (see [`crate::infra::platform_scope`]) — its SDK has no
/// REST projection yet, so it cannot carry a `consumes` declaration until it
/// does, and in the Embedded profile R1 is limited to the two paths coincide.
/// The resolver over the concrete repositories, as the gear and its REST
/// surface share it.
pub type ConcreteResolver = crate::domain::resolution::ValueResolver<
    crate::infra::storage::declaration_repo::DeclarationRepo,
    crate::infra::storage::value_repo::ValueRepo,
    crate::infra::storage::access_repo::AccessRepo,
>;

#[toolkit::consumes(contract = authz_resolver_sdk::AuthZResolverApi, from = "authz-resolver")]
#[toolkit::gear(
    name = "settings-service",
    deps = [types_registry, credstore],
    capabilities = [db, rest, stateful],
    lifecycle(entry = "serve", stop_timeout = "30s", await_ready)
)]
pub struct SettingsService {
    config: OnceLock<Arc<SettingsServiceConfig>>,
    db: OnceLock<Arc<DBProvider<DbError>>>,
    enforcer: OnceLock<Arc<PolicyEnforcer>>,
    names: OnceLock<Arc<crate::infra::actor_names::ActorNameResolver>>,
    security: OnceLock<Arc<dyn crate::domain::ports::SecurityMetrics>>,
    types: OnceLock<Arc<dyn TypesRegistryClient>>,
    validator: OnceLock<Arc<dyn TypeValidator>>,
    categories: OnceLock<
        Arc<
            crate::domain::category::CategoryService<
                crate::infra::storage::category_repo::CategoryRepo,
                crate::infra::storage::audit_store::AuditStore,
            >,
        >,
    >,
    resolver: OnceLock<Arc<ConcreteResolver>>,
    writes: OnceLock<Arc<crate::infra::value_writes::WriteCoordinator>>,
    access: OnceLock<Arc<crate::api::rest::access_handlers::ConcreteAccessService>>,
    hierarchy: OnceLock<Arc<dyn crate::domain::resolution::TenantHierarchy>>,
    declarations: OnceLock<
        Arc<
            crate::domain::declaration::DeclarationService<
                crate::infra::storage::declaration_repo::DeclarationRepo,
            >,
        >,
    >,
    declaration_admin:
        OnceLock<Arc<crate::api::rest::declaration_handlers::ConcreteDeclarationAdmin>>,
}

impl Default for SettingsService {
    fn default() -> Self {
        Self {
            config: OnceLock::new(),
            db: OnceLock::new(),
            enforcer: OnceLock::new(),
            names: OnceLock::new(),
            security: OnceLock::new(),
            types: OnceLock::new(),
            validator: OnceLock::new(),
            categories: OnceLock::new(),
            resolver: OnceLock::new(),
            writes: OnceLock::new(),
            access: OnceLock::new(),
            hierarchy: OnceLock::new(),
            declarations: OnceLock::new(),
            declaration_admin: OnceLock::new(),
        }
    }
}

/// How often the managed lifecycle releases the staged secrets nobody claimed.
const SWEEP_TICK: Duration = Duration::from_mins(1);

/// An interval that delays rather than bursts after a missed tick.
fn ticking(period: Duration) -> tokio::time::Interval {
    let mut interval = tokio::time::interval(period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    interval
}

/// How often the managed lifecycle prunes audit records past their retention
/// horizon. A horizon is at least twelve months away, so once a day loses
/// nothing and keeps the pass cheap.
const RETENTION_TICK: Duration = Duration::from_hours(24);

/// How the retention pass takes a backlog: batches of `size`, at most
/// `per_tick` of them a tick, each its own short statement and commit.
#[derive(Debug, Clone, Copy)]
struct PruneBatches {
    size: u64,
    per_tick: u32,
}

/// Ten thousand a batch and a hundred batches a tick: a million records, about
/// seven days' worth at the declared bound of fifty million a year, so a
/// backlog after a long outage clears in days while no statement runs long.
const PRUNE_BATCHES: PruneBatches = PruneBatches {
    size: 10_000,
    per_tick: 100,
};

/// The inventory gauges' samples: declarations by `(source, status)` and values
/// by `(scope_class, scope_kind)`, each with its count.
type InventoryCounts = (
    Vec<(&'static str, &'static str, u64)>,
    Vec<(&'static str, &'static str, u64)>,
);

/// How often the managed lifecycle refreshes the gauges: the needs-review
/// counts and the two grouped inventory counts (declarations by source and
/// status; values by scope class, one query per scope kind), each bounded by
/// the catalogue size, cheap enough for a minute's resolution on a dashboard.
/// The tick also re-records `settings_service_up` as a heartbeat.
const REVIEW_TICK: Duration = Duration::from_mins(1);

/// Publishes `settings_service_up` 0 when the lifecycle future is left by any
/// path: a clean stop, a panic, or the future being dropped.
struct ServiceDownGuard(Arc<dyn crate::domain::ports::LifecycleMetrics>);

impl Drop for ServiceDownGuard {
    fn drop(&mut self) {
        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-13
        self.0.service_up(false);
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-13
    }
}

impl SettingsService {
    /// The managed lifecycle: the pending-secret sweep and the needs-review
    /// gauge once a minute, and the audit retention pass once a day, until
    /// cancelled. The only long-running
    /// work this gear owns; everything else is request-driven.
    #[allow(
        clippy::redundant_pub_crate,
        reason = "module-private serve entry-point invoked by the toolkit runtime"
    )]
    pub(crate) async fn serve(
        self: Arc<Self>,
        cancel: CancellationToken,
        ready: ReadySignal,
    ) -> anyhow::Result<()> {
        let writes = self.writes()?;
        let db = self.db()?;
        let retention = Duration::from_hours(u64::from(self.config()?.audit_retention_days) * 24);
        let metrics: Arc<dyn crate::domain::ports::LifecycleMetrics> =
            Arc::new(crate::infra::lifecycle_metrics::OtelLifecycleMetrics::new());
        Self::run_lifecycle(&writes, &db, retention, metrics, ready, &cancel).await;
        Ok(())
    }

    /// The lifecycle from the ready signal to the cancellation, over the ports
    /// it reports to.
    async fn run_lifecycle(
        writes: &crate::infra::value_writes::WriteCoordinator,
        db: &DBProvider<DbError>,
        retention: Duration,
        metrics: Arc<dyn crate::domain::ports::LifecycleMetrics>,
        ready: ReadySignal,
        cancel: &CancellationToken,
    ) {
        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-13
        // Up before the signal: the gauge is 1 when the lifecycle starts. The
        // guard publishes 0 on every way out, a panic or an abort of this
        // future included; a clean stop publishes it once, from the guard.
        metrics.service_up(true);
        let _down = ServiceDownGuard(Arc::clone(&metrics));
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-13
        ready.notify();
        info!(
            tick_secs = SWEEP_TICK.as_secs(),
            "pending-secret sweep and audit retention started"
        );
        Self::tick_until_cancelled(writes, db, retention, metrics.as_ref(), cancel).await;
        info!("pending-secret sweep and audit retention stopped");
    }

    /// One refresh of the two inventory gauges, logged and never fatal: the
    /// declarations by source and status, and the values by scope class and
    /// kind, one grouped count each. Every combination is published, zero
    /// included, so an emptied bucket reads zero rather than its last count; a
    /// failed refresh publishes nothing and the gauges keep their last values
    /// until the next tick.
    async fn inventory_once(
        db: &DBProvider<DbError>,
        root: uuid::Uuid,
        metrics: &dyn crate::domain::ports::LifecycleMetrics,
    ) {
        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-14
        match Self::count_inventory(db, root).await {
            Ok((declarations, values)) => {
                for (source, status, count) in declarations {
                    metrics.declarations(source, status, count);
                }
                for (scope_class, scope_kind, count) in values {
                    metrics.values(scope_class, scope_kind, count);
                }
            }
            Err(err) => tracing::warn!(
                err = %LogSafe(&err),
                "inventory gauge refresh failed; retried next tick"
            ),
        }
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-14
    }

    /// Both inventory counts, zero-filled over their finite label sets, or the
    /// failure of either.
    async fn count_inventory(
        db: &DBProvider<DbError>,
        root: uuid::Uuid,
    ) -> Result<InventoryCounts, crate::domain::error::DomainError> {
        use crate::domain::declaration::{DeclarationRepository as _, SOURCES, STATUSES};
        use crate::domain::resolution::{scope_class, scope_kind};
        use crate::domain::value::ValueRepository as _;
        let conn = db.conn().map_err(|err| {
            crate::domain::error::DomainError::dependency_unavailable(
                "database",
                "open a connection",
                err,
            )
        })?;
        let scope = toolkit_security::AccessScope::allow_all();
        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-14
        let declared = crate::infra::storage::declaration_repo::DeclarationRepo
            .count_by_source_status(&conn, &scope)
            .await?;
        let stored = crate::infra::storage::value_repo::ValueRepo
            .count_by_scope_kind(&conn, &scope, root)
            .await?;
        let mut declarations = Vec::with_capacity(SOURCES.len() * STATUSES.len());
        for source in SOURCES {
            for status in STATUSES {
                let count = declared
                    .get(&(source.to_owned(), status.to_owned()))
                    .copied()
                    .unwrap_or(0);
                declarations.push((source, status, count));
            }
        }
        let mut values = Vec::new();
        for class in [
            scope_class::GLOBAL,
            scope_class::CASCADING,
            scope_class::LOCAL,
        ] {
            for kind in [scope_kind::PLATFORM, scope_kind::TENANT] {
                let count = stored.get(&(class.to_owned(), kind)).copied().unwrap_or(0);
                values.push((class, kind, count));
            }
        }
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-14
        Ok((declarations, values))
    }

    /// The periodic passes, until the lifecycle is cancelled.
    async fn tick_until_cancelled(
        writes: &crate::infra::value_writes::WriteCoordinator,
        db: &DBProvider<DbError>,
        retention: Duration,
        metrics: &dyn crate::domain::ports::LifecycleMetrics,
        cancel: &CancellationToken,
    ) {
        let mut interval = ticking(SWEEP_TICK);
        let mut retention_interval = ticking(RETENTION_TICK);
        let mut review_interval = ticking(REVIEW_TICK);
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                _ = interval.tick() => Self::sweep_once(writes, cancel).await,
                _ = retention_interval.tick() => Self::retention_tick(db, retention, cancel, metrics).await,
                _ = review_interval.tick() => Self::review_tick(writes, db, metrics).await,
            }
        }
    }

    /// The review tick: the needs-review gauge and the inventory gauges, each
    /// refreshed on its own and none fatal to the other.
    async fn review_tick(
        writes: &crate::infra::value_writes::WriteCoordinator,
        db: &DBProvider<DbError>,
        metrics: &dyn crate::domain::ports::LifecycleMetrics,
    ) {
        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-13
        // A heartbeat: a reader of delta temporality keeps exporting the series.
        metrics.service_up(true);
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-13
        Self::review_once(db, metrics).await;
        match writes.writer().resolver().root_tenant().await {
            Ok(root) => Self::inventory_once(db, root, metrics).await,
            Err(err) => tracing::warn!(
                err = %LogSafe(&err),
                "inventory gauge refresh skipped: the root tenant is unknown; retried next tick"
            ),
        }
    }

    /// One refresh of the needs-review gauge, logged and never fatal. Every
    /// source is published, zero included, so a fixed backlog reads zero
    /// rather than its last count; a failed pass leaves the gauge as it was
    /// until the next tick.
    async fn review_once(
        db: &DBProvider<DbError>,
        metrics: &dyn crate::domain::ports::LifecycleMetrics,
    ) {
        match Self::count_needs_review(db).await {
            Ok(counts) => {
                for (source, count) in counts {
                    metrics.needs_review(source, count);
                }
            }
            Err(err) => tracing::warn!(
                err = %LogSafe(&err),
                "needs-review gauge refresh failed; retried next tick"
            ),
        }
    }

    /// The flagged overrides per declaration source, across every tenant.
    async fn count_needs_review(
        db: &DBProvider<DbError>,
    ) -> Result<Vec<(&'static str, u64)>, crate::domain::error::DomainError> {
        use crate::domain::value::ValueRepository as _;
        let conn = db.conn().map_err(|err| {
            crate::domain::error::DomainError::dependency_unavailable(
                "database",
                "open a connection",
                err,
            )
        })?;
        let scope = toolkit_security::AccessScope::allow_all();
        let mut counts = Vec::with_capacity(crate::domain::declaration::SOURCES.len());
        for source in crate::domain::declaration::SOURCES {
            let count = crate::infra::storage::value_repo::ValueRepo
                .count_flagged(&conn, &scope, source)
                .await?;
            counts.push((source, count));
        }
        Ok(counts)
    }

    /// The daily tick: one retention pass against the clock now.
    async fn retention_tick(
        db: &DBProvider<DbError>,
        retention: Duration,
        cancel: &CancellationToken,
        metrics: &dyn crate::domain::ports::LifecycleMetrics,
    ) {
        Self::prune_once(
            db,
            retention,
            time::OffsetDateTime::now_utc(),
            PRUNE_BATCHES,
            cancel,
            metrics,
        )
        .await;
    }

    /// One audit retention pass, logged and never fatal: records past their
    /// horizon leave — an explicit `retain_until`, or `occurred_at` plus the
    /// configured default — batch by batch, until a batch comes back short,
    /// the tick's cap is reached or the lifecycle is stopped. What the pass
    /// did not reach, or a failed batch left, waits for the next tick; what
    /// earlier batches deleted stays deleted. Returns how many records went.
    async fn prune_once(
        db: &DBProvider<DbError>,
        default_retention: Duration,
        now: time::OffsetDateTime,
        batches: PruneBatches,
        cancel: &CancellationToken,
        metrics: &dyn crate::domain::ports::LifecycleMetrics,
    ) -> u64 {
        let (pruned, failure) = Self::prune(db, default_retention, now, batches, cancel).await;
        // Reported whatever happened: a failed pass and one with nothing to
        // prune both return zero, and only this tells them apart.
        metrics.retention_pass(
            if failure.is_some() {
                crate::domain::ports::labels::RETENTION_FAILED
            } else {
                crate::domain::ports::labels::RETENTION_OK
            },
            pruned,
        );
        if pruned > 0 {
            info!(pruned, "audit records past their retention horizon pruned");
        }
        if let Some(err) = failure {
            tracing::warn!(
                pruned,
                err = %LogSafe(&err),
                "audit retention pass failed; the rest is retried next tick"
            );
        }
        pruned
    }

    /// One pass of the sweep, logged and never fatal: what it could not
    /// release waits for the next tick.
    async fn sweep_once(
        writes: &crate::infra::value_writes::WriteCoordinator,
        cancel: &CancellationToken,
    ) {
        // @cpt-begin:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-9
        match writes
            .sweep_expired(crate::infra::value_writes::SWEEP_LIMIT, cancel)
            .await
        {
            Ok(0) => {}
            Ok(released) => info!(released, "expired staged secrets released"),
            Err(err) => tracing::warn!(
                err = %LogSafe(&err),
                "pending-secret sweep failed; retried next tick"
            ),
        }
        // @cpt-end:cpt-cf-settings-service-flow-secret-values-stage:p1:inst-sv-stage-9
    }

    /// The retention pass itself: how many records it pruned, and the failure
    /// that stopped it early, for the caller to log.
    async fn prune(
        db: &DBProvider<DbError>,
        default_retention: Duration,
        now: time::OffsetDateTime,
        batches: PruneBatches,
        cancel: &CancellationToken,
    ) -> (u64, Option<crate::domain::error::DomainError>) {
        let Ok(retention) = time::Duration::try_from(default_retention) else {
            return (
                0,
                Some(crate::domain::error::DomainError::Internal {
                    diagnostic: "the audit retention does not fit a timestamp span".to_owned(),
                }),
            );
        };
        let conn = match db.conn() {
            Ok(conn) => conn,
            Err(err) => {
                return (
                    0,
                    Some(crate::domain::error::DomainError::dependency_unavailable(
                        "database",
                        "open a connection",
                        err,
                    )),
                );
            }
        };
        let scope = toolkit_security::AccessScope::allow_all();
        // The configured retention first, where the trigger reads it: the
        // database then refuses deleting anything younger by any path, and a
        // write that fails fails the pass, which the pass metric reports.
        let days =
            u32::try_from(default_retention.as_secs().div_euclid(86_400)).unwrap_or(u32::MAX);
        if let Err(err) =
            crate::infra::storage::audit_store::AuditStore::record_retention(&conn, &scope, days)
                .await
        {
            return (0, Some(err));
        }
        let mut pruned = 0;
        for _ in 0..batches.per_tick {
            if cancel.is_cancelled() {
                break;
            }
            match crate::infra::storage::audit_store::AuditStore::prune_expired(
                &conn,
                &scope,
                now,
                retention,
                batches.size,
            )
            .await
            {
                Ok(batch) => {
                    pruned += batch;
                    if batch < batches.size {
                        break;
                    }
                }
                Err(err) => return (pruned, Some(err)),
            }
        }
        (pruned, None)
    }

    /// The bootstrap configuration, once initialization has run.
    ///
    /// # Errors
    /// Returns an error when called before [`Gear::init`].
    pub fn config(&self) -> anyhow::Result<Arc<SettingsServiceConfig>> {
        self.config
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{} gear not initialized", Self::MODULE_NAME))
    }

    /// The database handle, once initialization has run.
    ///
    /// # Errors
    /// Returns an error when called before [`Gear::init`].
    pub fn db(&self) -> anyhow::Result<Arc<DBProvider<DbError>>> {
        self.db
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{} gear not initialized", Self::MODULE_NAME))
    }

    /// The authorization enforcement point, once initialization has run.
    ///
    /// Every handler obtains its `AccessScope` through this rather than
    /// consulting the decision point directly, so the fail-closed projection in
    /// [`crate::api::authz`] cannot be bypassed by a handler that forgets it.
    /// The enforcer itself resolves the decision point lazily from the hub: the
    /// resolver is a consumed client, wired after init (DESIGN.md §4.9), and a
    /// decision it cannot obtain is a denial, not an allow.
    ///
    /// # Errors
    /// Returns an error when called before [`Gear::init`].
    pub fn enforcer(&self) -> anyhow::Result<Arc<PolicyEnforcer>> {
        self.enforcer
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{} gear not initialized", Self::MODULE_NAME))
    }

    /// The actor name resolver, once initialization has run.
    ///
    /// # Errors
    /// Returns an error when called before [`Gear::init`].
    pub fn names(&self) -> anyhow::Result<Arc<crate::infra::actor_names::ActorNameResolver>> {
        self.names
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{} gear not initialized", Self::MODULE_NAME))
    }

    /// The security metrics port, once initialization has run.
    ///
    /// # Errors
    /// Returns an error when called before [`Gear::init`].
    pub fn security(&self) -> anyhow::Result<Arc<dyn crate::domain::ports::SecurityMetrics>> {
        self.security
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{} gear not initialized", Self::MODULE_NAME))
    }

    /// The GTS types registry, once initialization has run.
    ///
    /// A declaration's value type lives in the registry, not here: the read
    /// surface resolves its trait set for rendering, and declaration creation
    /// checks the type is a real catalogue entry. `has_secret_trait` is
    /// denormalised onto the row for masking precisely so that hot path does
    /// *not* come back through this client.
    ///
    /// # Errors
    /// Returns an error when called before [`Gear::init`].
    pub fn types(&self) -> anyhow::Result<Arc<dyn TypesRegistryClient>> {
        self.types
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{} gear not initialized", Self::MODULE_NAME))
    }
    /// The Type Validator, once initialization has run.
    ///
    /// Generic over any GTS type id; for a setting the id passed is the
    /// declaration's `value_type_id`. Every rule it enforces is hard, and a type
    /// it cannot resolve is a rejection rather than a vacuous pass.
    ///
    /// # Errors
    /// Returns an error when called before [`Gear::init`].
    pub fn validator(&self) -> anyhow::Result<Arc<dyn TypeValidator>> {
        self.validator
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{} gear not initialized", Self::MODULE_NAME))
    }

    /// The Value Resolver, once initialized.
    ///
    /// # Errors
    /// If called before `init` completed.
    pub fn resolver(&self) -> anyhow::Result<Arc<ConcreteResolver>> {
        self.resolver
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{} resolver not initialized", Self::MODULE_NAME))
    }

    /// The write coordinator, once initialized.
    ///
    /// # Errors
    /// If called before `init` completed.
    pub fn writes(&self) -> anyhow::Result<Arc<crate::infra::value_writes::WriteCoordinator>> {
        self.writes
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{} writes not initialized", Self::MODULE_NAME))
    }

    /// The tenant access service, once initialized.
    ///
    /// # Errors
    /// If called before `init` completed.
    pub fn access(
        &self,
    ) -> anyhow::Result<Arc<crate::api::rest::access_handlers::ConcreteAccessService>> {
        self.access
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{} access not initialized", Self::MODULE_NAME))
    }

    /// The tenant hierarchy port, once initialized.
    ///
    /// # Errors
    /// If called before `init` completed.
    pub fn hierarchy(&self) -> anyhow::Result<Arc<dyn crate::domain::resolution::TenantHierarchy>> {
        self.hierarchy
            .get()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{} hierarchy not initialized", Self::MODULE_NAME))
    }
}

impl SettingsService {
    /// Refuse a configuration the gear cannot run under.
    fn check_config(config: &SettingsServiceConfig) -> anyhow::Result<()> {
        // The two SDK traits are bound in process and have no remote contract
        // to reach, so a deployment that wires one remotely is refused here
        // rather than left with a binding that resolves to nothing.
        if let Err(reason) = config.check_in_process_bindings() {
            anyhow::bail!("{}: {reason}", Self::MODULE_NAME);
        }
        if config.audit_retention_days < crate::audit::MIN_RETENTION_DAYS {
            anyhow::bail!(
                "{}: audit_retention_days is {} but must not be below {}",
                Self::MODULE_NAME,
                config.audit_retention_days,
                crate::audit::MIN_RETENTION_DAYS
            );
        }
        Ok(())
    }
}

#[async_trait]
impl Gear for SettingsService {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-1
        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-2
        // `config`, not `config_or_default`. Bootstrap values are
        // deployment-owned and are never managed settings, so there is nothing
        // to fall back to: an absent required value fails startup here rather
        // than surfacing later as the service enforcing something nobody chose.
        let config: SettingsServiceConfig = ctx.config()?;
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-2
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-1

        self.config
            .set(Arc::new(config))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-3
        let db: Arc<DBProvider<DbError>> = Arc::new(ctx.db_required()?);
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-3

        self.db
            .set(db)
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-6
        // The one client called during our own init, and therefore the one
        // `deps` entry: registering the settings GTS schemas is a real call into
        // the registry, so it must already be up. A registry that is absent must
        // not first be discovered by a read that has already passed
        // authorization and reached the database.
        let types = ctx
            .client_hub()
            .get::<dyn TypesRegistryClient>()
            .map_err(|e| anyhow::anyhow!("failed to resolve the types registry: {e}"))?;
        self.types
            .set(types)
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        // Built over the registry client above: validation of a value against
        // its type is the one path that consults the registry per call, and it
        // fails closed on a type the registry does not know.
        self.validator
            .set(Arc::new(
                crate::infra::type_validator::GtsTypeValidator::new(self.types()?),
            ))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // Consumed, not depended on. The authorization resolver is declared with
        // `#[toolkit::consumes]` on the struct and wired by the proxy-wiring
        // phase *after* init, so resolving it here would fail by construction;
        // the enforcer fetches it from the hub per call and denies when it
        // cannot. The tenant resolver is fetched the same way, on the first
        // platform-scoped mutation that needs the root tenant's id. Neither is
        // an ordering claim on the rest of the platform (DESIGN.md §4.9).
        let hub = ctx.client_hub();
        self.enforcer
            .set(Arc::new(PolicyEnforcer::from_hub(Arc::clone(&hub))))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        // Actor names on the history read: AM is found in the hub at each pass.
        self.names
            .set(Arc::new(
                crate::infra::actor_names::ActorNameResolver::from_hub(
                    Arc::clone(&hub),
                    Arc::new(crate::infra::actor_name_metrics::OtelActorNameMetrics::new()),
                ),
            ))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        // The metrics ports the REST surface, the secret path and the audit
        // sink report to, each bound once and shared.
        let security: Arc<dyn crate::domain::ports::SecurityMetrics> =
            Arc::new(crate::infra::security_metrics::OtelSecurityMetrics::new());
        self.security
            .set(Arc::clone(&security))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        let read_metrics: Arc<dyn crate::domain::ports::ReadMetrics> =
            Arc::new(crate::infra::read_metrics::OtelReadMetrics::new());
        let platform_scope: Arc<dyn PlatformScope> =
            Arc::new(crate::infra::platform_scope::HubPlatformScope::new(hub));
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-6

        // The audit sink: the gear's own table, written in each mutation's
        // transaction. The retention default is validated here because a store
        // configured below twelve months would prune what the platform must keep.
        Self::check_config(self.config()?.as_ref())?;
        let audit = crate::infra::storage::audit_store::AuditStore::new(Arc::clone(&security));
        self.categories
            .set(Arc::new(crate::domain::category::CategoryService::new(
                crate::infra::storage::category_repo::CategoryRepo,
                audit.clone(),
            )))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // The contribution door: gears register their declarations through this
        // trait from their own init, so it is bound into the hub here and each
        // caller names `settings-service` in its `deps` to initialize after us.
        // The read path: the local effective-value cache — this gear's
        // `cache_ttl_seconds` and `cache_max_entries` are its knobs — the
        // tenant hierarchy port over the tenant resolver, the resolver over
        // both repositories, and the in-process reader bound into the hub for
        // every consuming gear.
        let config = self.config()?;
        if config.cache_max_entries == 0 {
            return Err(anyhow::anyhow!(
                "cache_max_entries must be at least 1: a cache that holds nothing puts every read \
                 on the database"
            ));
        }
        // The TTL is the design's ceiling on staleness, not the operator's:
        // a deployment may shorten the backstop, never widen it, and a zero
        // would put every read on the database like a cache of no entries.
        let ttl_ceiling = crate::domain::resolution::EffectiveCache::TTL_CEILING.as_secs();
        if config.cache_ttl_seconds == 0 || config.cache_ttl_seconds > ttl_ceiling {
            anyhow::bail!(
                "{}: cache_ttl_seconds is {} but must be between 1 and {}, the design's backstop \
                 on how stale a replica may serve after a missed invalidation",
                Self::MODULE_NAME,
                config.cache_ttl_seconds,
                ttl_ceiling
            );
        }
        let cache = Arc::new(crate::domain::resolution::EffectiveCache::bounded(
            std::time::Duration::from_secs(config.cache_ttl_seconds),
            config.cache_max_entries,
        ));
        let hierarchy: Arc<dyn crate::domain::resolution::TenantHierarchy> = Arc::new(
            crate::infra::tenant_hierarchy::HubTenantHierarchy::new(ctx.client_hub()),
        );
        self.hierarchy
            .set(Arc::clone(&hierarchy))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        let hierarchy_for_access = Arc::clone(&hierarchy);
        let resolver = Arc::new(crate::domain::resolution::ValueResolver::new(
            crate::infra::storage::declaration_repo::DeclarationRepo,
            crate::infra::storage::value_repo::ValueRepo,
            crate::infra::storage::access_repo::AccessRepo,
            hierarchy,
            Arc::clone(&platform_scope),
            self.validator()?,
            Arc::clone(&cache),
        ));
        self.resolver
            .set(Arc::clone(&resolver))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        // Tenant access restrictions: set, clear, read and list, each mutation in
        // its own transaction with its record, evicting the restricted subtree.
        self.access
            .set(Arc::new(crate::domain::access::AccessService::new(
                crate::infra::storage::declaration_repo::DeclarationRepo,
                crate::infra::storage::access_repo::AccessRepo,
                crate::infra::storage::audit_store::AuditStore::new(Arc::clone(&security)),
                Arc::clone(&hierarchy_for_access),
                Arc::clone(&platform_scope),
                Arc::clone(&cache),
            )))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // The write path: the step-up verifier over the platform's AuthN
        // resolver, fetched from the hub at first use — never here, and the
        // resolver is not in `deps` (DESIGN.md §4.9). Always bound: an absent
        // `step_up` section is the default policy, not the absence of a
        // verifier. Init refuses a window above five minutes.
        let step_up: Arc<dyn crate::domain::stepup::StepUpVerifier> =
            Arc::new(crate::infra::step_up::AuthnStepUpVerifier::from_config(
                ctx.client_hub(),
                &config.step_up,
            )?);
        // The Secret Manager over the Credential Store: `credstore` is a system
        // gear, so its client is in the hub before this init runs.
        let credstore = ctx
            .client_hub()
            .get::<dyn credstore_sdk::CredStoreClientV1>()
            .map_err(|e| anyhow::anyhow!("{}: credstore client: {e}", Self::MODULE_NAME))?;
        let secrets: Arc<dyn crate::domain::ports::SecretManager> = Arc::new(
            crate::infra::secret_manager::CredStoreSecretManager::new(credstore),
        );
        let step_up_for_admin = Arc::clone(&step_up);
        // One counter set for the write path and the declaration admin, so
        // `settings_step_up_total` is one series per operation.
        let write_metrics: Arc<dyn crate::domain::ports::WriteMetrics> =
            Arc::new(crate::infra::write_metrics::OtelWriteMetrics::new());
        let secrets_for_admin = Arc::clone(&secrets);
        let writer = Arc::new(crate::domain::writes::ValueWriter::new(
            crate::infra::storage::value_repo::ValueRepo,
            Arc::clone(&resolver),
            self.validator()?,
            crate::infra::storage::audit_store::AuditStore::new(Arc::clone(&security)),
            step_up,
            Arc::clone(&secrets),
            crate::infra::storage::pending_secret_repo::PendingSecretRepo,
            Arc::new(crate::infra::write_metrics::LoggingPublisher),
            Arc::clone(&write_metrics),
        ));
        self.writes
            .set(Arc::new(crate::infra::value_writes::WriteCoordinator::new(
                self.db()?,
                writer,
                crate::infra::storage::audit_store::AuditStore::new(Arc::clone(&security)),
            )))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-7
        // The machine-only plaintext path behind the reader: per-setting PEP
        // gate, the Secret Manager, and the audit store for `secret_use`.
        let secret_resolver = Arc::new(crate::domain::secrets::SecretResolver::new(
            Arc::clone(&resolver),
            Arc::clone(&secrets),
            Arc::new(crate::infra::secret_manager::PepSecretGate::new(
                self.enforcer()?,
                Arc::clone(&security),
            )),
            crate::infra::storage::audit_store::AuditStore::new(Arc::clone(&security)),
            Arc::clone(&security),
        ));
        let reader: Arc<dyn SettingsReaderClient> =
            Arc::new(crate::infra::reader_client::ReaderClient::new(
                self.db()?,
                Arc::clone(&resolver),
                secret_resolver,
                read_metrics,
            ));
        ctx.client_hub()
            .register::<dyn SettingsReaderClient>(reader);

        let contributions = Arc::new(crate::domain::contribution::ContributionService::new(
            crate::infra::storage::declaration_repo::DeclarationRepo,
            crate::infra::storage::category_repo::CategoryRepo,
            crate::infra::storage::value_repo::ValueRepo,
            self.validator()?,
            Arc::new(
                crate::infra::setting_type_registrar::TypesRegistryRegistrar::new(self.types()?),
            ),
            audit,
        ));
        let contribution_client: Arc<dyn SettingsContributionClient> =
            Arc::new(crate::infra::contribution_client::ContributionClient::new(
                self.db()?,
                contributions,
                Arc::clone(&cache),
                Arc::new(crate::infra::write_metrics::LoggingPublisher),
            ));
        ctx.client_hub()
            .register::<dyn SettingsContributionClient>(contribution_client);
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-7

        self.declarations
            .set(Arc::new(
                crate::domain::declaration::DeclarationService::new(
                    crate::infra::storage::declaration_repo::DeclarationRepo,
                    self.types()?,
                ),
            ))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // Administrative authoring: create, revive, edit metadata, retire. It
        // shares the step-up verifier and the audit store with the value write
        // path, so one gate and one trail cover both.
        self.declaration_admin
            .set(Arc::new(crate::domain::declaration::DeclarationAdmin::new(
                crate::infra::storage::declaration_repo::DeclarationRepo,
                crate::infra::storage::category_repo::CategoryRepo,
                crate::infra::storage::value_repo::ValueRepo,
                self.validator()?,
                Arc::new(
                    crate::infra::setting_type_registrar::TypesRegistryRegistrar::new(
                        self.types()?,
                    ),
                ),
                Arc::clone(&step_up_for_admin),
                crate::infra::storage::audit_store::AuditStore::new(Arc::clone(&security)),
                Arc::clone(&cache),
                crate::infra::storage::pending_secret_repo::PendingSecretRepo,
                secrets_for_admin,
                write_metrics,
            )))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-12
        info!("Settings Service gear initialized");
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-12

        Ok(())
    }
}

impl RestApiCapability for SettingsService {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        let service = self
            .categories
            .get()
            .ok_or_else(|| anyhow::anyhow!("category service not initialized"))?
            .clone();
        let declarations = self
            .declarations
            .get()
            .ok_or_else(|| anyhow::anyhow!("declaration service not initialized"))?
            .clone();
        let router = crate::api::rest::routes::register_routes(
            router,
            openapi,
            service,
            self.resolver()?,
            self.db()?,
            self.enforcer()?,
        );
        let admin = self
            .declaration_admin
            .get()
            .ok_or_else(|| anyhow::anyhow!("declaration admin not initialized"))?
            .clone();
        let router = crate::api::rest::declaration_routes::register_routes(
            router,
            openapi,
            declarations,
            admin,
            self.resolver()?,
            self.db()?,
            self.enforcer()?,
        );
        let router = crate::api::rest::setting_routes::register_routes(
            router,
            openapi,
            self.resolver()?,
            self.db()?,
            self.enforcer()?,
            self.names()?,
        );
        let router = crate::api::rest::value_routes::register_routes(
            router,
            openapi,
            self.writes()?,
            self.enforcer()?,
        );
        let router = crate::api::rest::access_routes::register_routes(
            router,
            openapi,
            self.access()?,
            self.db()?,
            self.enforcer()?,
        );
        // Search: the dialect is the database's, decided here once, where the
        // provider is at hand; the service is otherwise stateless.
        let db = self.db()?;
        let search = Arc::new(crate::domain::search::service::SearchService::new(
            crate::infra::storage::search_repo::SearchRepo::new(db.db().backend()),
        ));
        let router = crate::api::rest::search_routes::register_routes(
            router,
            openapi,
            search,
            self.resolver()?,
            db,
            self.enforcer()?,
        );
        // The security-metrics port, beside the enforcer every route carries:
        // each handler that asks for an access scope counts its refusals on it.
        Ok(router.layer(axum::Extension(self.security()?)))
    }
}

impl DatabaseCapability for SettingsService {
    // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-4
    // @cpt-begin:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-5
    /// The gear's migrations, run to completion before it serves.
    ///
    /// `ToolKit` runs whatever is outstanding here and aborts startup if one
    /// fails, so steps 4 and 5 of gear init are satisfied by handing over the
    /// list rather than by driving it here — and a partially migrated schema is
    /// unreachable because no request is accepted until every migration has
    /// succeeded.
    fn migrations(&self) -> Vec<Box<dyn MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        crate::infra::storage::migrations::Migrator::migrations()
    }
    // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-5
    // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-4
}

#[cfg(test)]
#[path = "gear_tests.rs"]
mod gear_tests;
