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
/// the enforcer fetches it from the hub on first use. The Credential Store is
/// in neither `deps` nor `consumes`: it is an ordinary application gear, so
/// its init order relative to this gear is not guaranteed. With
/// `secret_storage: required` (the default) its client is looked up in the hub
/// at first use and kept, and `post_init`, which the runtime calls after every
/// gear of the group has initialized, refuses to start without it; with
/// `disabled` the gear does not look for
/// it (see [`crate::infra::secret_manager`]). The tenant resolver is
/// fetched from the hub on first use as well (see [`crate::infra::platform_scope`]) — its SDK has no
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
    deps = [types_registry],
    capabilities = [db, rest, stateful],
    lifecycle(entry = "serve", stop_timeout = "30s", await_ready)
)]
pub struct SettingsService {
    config: OnceLock<Arc<SettingsServiceConfig>>,
    db: OnceLock<Arc<DBProvider<DbError>>>,
    enforcer: OnceLock<Arc<PolicyEnforcer>>,
    names: OnceLock<Arc<crate::infra::actor_names::ActorNameResolver>>,
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

/// How often the managed lifecycle prunes audit records past their retention
/// horizon. A horizon is at least twelve months away, so once a day loses
/// nothing and keeps the pass cheap.
const RETENTION_TICK: Duration = Duration::from_hours(24);

/// How often the managed lifecycle refreshes the needs-review gauge: two
/// indexed counts, cheap enough for a minute's resolution on a dashboard.
const REVIEW_TICK: Duration = Duration::from_mins(1);

/// An interval that delays rather than bursts after a missed tick.
fn ticking(period: Duration) -> tokio::time::Interval {
    let mut interval = tokio::time::interval(period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    interval
}

/// The loops this replica runs, from the `background` switches.
///
/// A switched-off loop has no interval at all, so it never ticks, the first
/// immediate tick included: nothing about it runs on this replica, and the
/// pass is the operator's to request through the maintenance routes.
struct Loops {
    sweep: Option<tokio::time::Interval>,
    retention: Option<tokio::time::Interval>,
    review: Option<tokio::time::Interval>,
}

impl Loops {
    fn from_config(background: crate::config::BackgroundConfig) -> Self {
        Self {
            sweep: background.pending_secret_sweep.then(|| ticking(SWEEP_TICK)),
            retention: background.audit_retention.then(|| ticking(RETENTION_TICK)),
            review: background.needs_review_gauge.then(|| ticking(REVIEW_TICK)),
        }
    }
}

/// The next tick of a loop, or never when the loop is switched off.
async fn next_tick(interval: &mut Option<tokio::time::Interval>) {
    match interval {
        Some(interval) => {
            interval.tick().await;
        }
        None => std::future::pending().await,
    }
}

impl SettingsService {
    /// The Secret Manager over the Credential Store. The store is in neither
    /// `deps` nor `consumes`: it is an ordinary application gear, so its client
    /// may not be in the hub yet during this gear's init. `required` builds the
    /// manager over the hub, which looks the client up at first use; that the
    /// client is there is checked by [`Gear::post_init`]. `disabled` does not
    /// look at all.
    fn secret_manager(
        ctx: &GearCtx,
        config: &SettingsServiceConfig,
    ) -> Arc<dyn crate::domain::ports::SecretManager> {
        match config.secret_storage {
            crate::config::SecretStorage::Required => Arc::new(
                crate::infra::secret_manager::CredStoreSecretManager::from_hub(ctx.client_hub()),
            ),
            crate::config::SecretStorage::Disabled => {
                tracing::warn!(
                    "secret storage is disabled: secret settings are refused with \
                     `secret storage is not configured`"
                );
                Arc::new(crate::infra::secret_manager::CredStoreSecretManager::disabled())
            }
        }
    }

    /// The managed lifecycle: the pending-secret sweep and the needs-review
    /// gauge once a minute, and the audit retention pass once a day, each
    /// only if `background` leaves it on, until cancelled. The only
    /// long-running work this gear owns; everything else is request-driven.
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
        let config = self.config()?;
        let retention = Duration::from_hours(u64::from(config.audit_retention_days) * 24);
        let loops = Loops::from_config(config.background);
        ready.notify();
        info!(
            sweep = config.background.pending_secret_sweep,
            retention = config.background.audit_retention,
            review = config.background.needs_review_gauge,
            "background passes started"
        );
        let metrics = crate::infra::lifecycle_metrics::OtelLifecycleMetrics::new();
        Self::tick_until_cancelled(&writes, &db, retention, &metrics, loops, &cancel).await;
        info!("background passes stopped");
        Ok(())
    }

    /// The periodic passes, until the lifecycle is cancelled. With every loop
    /// off this waits for the cancellation and does nothing else.
    async fn tick_until_cancelled(
        writes: &crate::infra::value_writes::WriteCoordinator,
        db: &DBProvider<DbError>,
        retention: Duration,
        metrics: &dyn crate::domain::ports::LifecycleMetrics,
        mut loops: Loops,
        cancel: &CancellationToken,
    ) {
        use crate::infra::maintenance;
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                () = next_tick(&mut loops.sweep) => maintenance::sweep_once(writes, cancel).await,
                () = next_tick(&mut loops.retention) => {
                    maintenance::retention_tick(db, retention, cancel, metrics).await;
                }
                () = next_tick(&mut loops.review) => maintenance::review_once(db, metrics).await,
            }
        }
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
        let platform_scope: Arc<dyn PlatformScope> =
            Arc::new(crate::infra::platform_scope::HubPlatformScope::new(hub));
        // @cpt-end:cpt-cf-settings-service-algo-gear-foundation-gear-init:p1:inst-gf-init-6

        // The audit sink: the gear's own table, written in each mutation's
        // transaction. The retention default is validated here because a store
        // configured below twelve months would prune what the platform must keep.
        let config = self.config()?;
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
        let audit = crate::infra::storage::audit_store::AuditStore;
        self.categories
            .set(Arc::new(crate::domain::category::CategoryService::new(
                crate::infra::storage::category_repo::CategoryRepo,
                audit,
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
                crate::infra::storage::audit_store::AuditStore,
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
        // The Secret Manager over the Credential Store, which it looks up at
        // first use (see `secret_manager`).
        let secrets = Self::secret_manager(ctx, &config);
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
            crate::infra::storage::audit_store::AuditStore,
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
            )),
            crate::infra::storage::audit_store::AuditStore,
        ));
        let reader: Arc<dyn SettingsReaderClient> =
            Arc::new(crate::infra::reader_client::ReaderClient::new(
                self.db()?,
                Arc::clone(&resolver),
                secret_resolver,
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
            Arc::clone(&secrets),
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
                crate::infra::storage::audit_store::AuditStore,
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

    /// With `secret_storage: required`, refuses to start when no Credential
    /// Store client is registered.
    ///
    /// The check is here and not in `init` because `credstore` is an ordinary
    /// application gear outside this gear's `deps`, so the order of their
    /// inits is not guaranteed. The runtime calls `post_init` after every gear
    /// of the group has initialized and before routes are mounted, so a host
    /// that carries credstore has its client in the hub by now. Until then,
    /// for instance in contributions made from other gears' `post_init`, the
    /// manager finds the client lazily.
    async fn post_init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let config = self.config()?;
        if config.secret_storage == crate::config::SecretStorage::Required {
            ctx.client_hub()
                .get::<dyn credstore_sdk::CredStoreClientV1>()
                .map_err(|e| {
                    anyhow::anyhow!(
                        "{}: secret_storage is `required` but no Credential Store client is \
                         registered ({e}); add the credstore gear to the host, or set \
                         `secret_storage: disabled` if this deployment stores no secrets",
                        Self::MODULE_NAME
                    )
                })?;
        }
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
        // Maintenance: one pass of the loops, asked for by an external
        // scheduler. Authenticated and internal, never `.exposed()`.
        let config = self.config()?;
        let router = crate::api::rest::maintenance_routes::register_routes(
            router,
            openapi,
            self.writes()?,
            self.db()?,
            Arc::new(
                crate::api::rest::maintenance_handlers::MaintenanceSettings {
                    retention: Duration::from_hours(u64::from(config.audit_retention_days) * 24),
                    batches: crate::infra::maintenance::REQUEST_BATCHES,
                    metrics: Arc::new(crate::infra::lifecycle_metrics::OtelLifecycleMetrics::new()),
                },
            ),
            self.resolver()?,
            self.enforcer()?,
        );
        // The anonymous read: the only route here reached without a token.
        let router = crate::api::rest::public_routes::register_routes(
            router,
            openapi,
            self.resolver()?,
            self.db()?,
        );
        // Search: the dialect is the database's, decided here once, where the
        // provider is at hand; the service is otherwise stateless.
        let db = self.db()?;
        let search = Arc::new(crate::domain::search::service::SearchService::new(
            crate::infra::storage::search_repo::SearchRepo::new(db.db().backend()),
        ));
        Ok(crate::api::rest::search_routes::register_routes(
            router,
            openapi,
            search,
            self.resolver()?,
            db,
            self.enforcer()?,
        ))
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
