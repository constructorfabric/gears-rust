//! Gear declaration for the Types Registry gear.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
use toolkit::contracts::{DatabaseCapability, SystemCapability};
use toolkit::lifecycle::ReadySignal;
use toolkit::{ClientHub, Gear, GearCtx, RestApiCapability};
use toolkit_contract::policy::PolicyStack;
use toolkit_db::outbox::OutboxHandle;
use toolkit_db::{DBProvider, DbError};
use toolkit_gts::{all_inventory_instances, all_inventory_type_schemas};
use tracing::{debug, info};
use types_registry_sdk::{
    PlatformTypesRegistryApi, RegisterResult, RegisterSummary, TypesRegistryApi,
    TypesRegistryClient,
};

use crate::config::TypesRegistryConfig;
use crate::domain::admission::OperationDispatch;
use crate::domain::legacy_local_client::TypesRegistryLocalClient;
use crate::domain::local_client::LocalClient;
use crate::domain::policy::RegistrationPolicy;
use crate::domain::ports::Stores;
use crate::domain::ports::metrics::AdmissionMetrics;
use crate::domain::registry_service::RegistryService;
use crate::domain::seeding::{SeedSet, seed};
use crate::domain::service::TypesRegistryService;
use crate::infra::InMemoryGtsRepository;
use crate::infra::outbox::{OutboxDispatch, TABLE_PREFIX as OUTBOX_TABLE_PREFIX};
use crate::infra::storage::Repos;

/// Types Registry gear: REST, managed storage, inventory seeding and admission worker.
///
/// Both contracts are provided locally and over REST (SPEC §8.4). The registry's own host
/// always wires them `Local` — its configuration admits no `client_wiring` — and only past
/// the seed barrier; a consumer elsewhere reaches them through the SDK's resolving clients.
#[toolkit::provides(
    contract = types_registry_sdk::PlatformTypesRegistryApi,
    local = Self::seeded_platform_api,
    transports = [local, rest],
    rest_client = types_registry_sdk::PlatformTypesRegistryApiRestClient,
)]
#[toolkit::provides(
    contract = types_registry_sdk::TypesRegistryApi,
    local = Self::seeded_tenant_api,
    transports = [local, rest],
    rest_client = types_registry_sdk::TypesRegistryApiRestClient,
)]
#[toolkit::gear(
    name = "types-registry",
    capabilities = [system, db, rest, stateful],
    // Leave five seconds before the host's hard shutdown deadline. An admission
    // cut short keeps its outbox message, so the next boot's lease redelivers it.
    lifecycle(entry = "serve", stop_timeout = "30s", await_ready)
)]
pub struct TypesRegistryGear {
    service: OnceLock<Arc<TypesRegistryService>>,
    /// The database-backed path. Absent when no database is bound to this gear.
    registry: OnceLock<Arc<RegistryService>>,
    local_client: OnceLock<Arc<TypesRegistryLocalClient>>,
    /// Pipeline retained for shutdown; the mutex guards only non-async moves.
    outbox: Mutex<Option<OutboxHandle>>,
}

impl Default for TypesRegistryGear {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
            registry: OnceLock::new(),
            local_client: OnceLock::new(),
            outbox: Mutex::new(None),
        }
    }
}

/// The seeded database-backed client, held in the `ClientHub` only while `init` wires the
/// two contracts: the `provides` factories receive a context, not the gear.
struct SeededClient(Arc<LocalClient>);

/// Register `value` in `hub` for the duration of `wire` only, and return what `wire`
/// returns: the entry is gone afterwards, whether wiring succeeded, failed, panicked or
/// was dropped unfinished.
async fn held_while<T: Send + Sync + 'static>(
    hub: &ClientHub,
    value: Arc<T>,
    wire: impl Future<Output = anyhow::Result<()>>,
) -> anyhow::Result<()> {
    /// Removes the held `T` from the hub when dropped.
    struct Held<'a, T: Send + Sync + 'static>(&'a ClientHub, std::marker::PhantomData<T>);

    impl<T: Send + Sync + 'static> Drop for Held<'_, T> {
        fn drop(&mut self) {
            self.0.remove::<T>();
        }
    }

    hub.register::<T>(value);
    let _held = Held::<T>(hub, std::marker::PhantomData);
    wire.await
}

impl TypesRegistryGear {
    fn seeded(ctx: &GearCtx) -> anyhow::Result<Arc<LocalClient>> {
        let seeded = ctx.client_hub().get::<SeededClient>().map_err(|_| {
            anyhow::anyhow!("types-registry wires its APIs only past the seed barrier")
        })?;
        Ok(Arc::clone(&seeded.0))
    }

    /// `provides` factory: the platform API over the seeded database.
    fn seeded_platform_api(
        ctx: &GearCtx,
        _policies: Arc<PolicyStack>,
    ) -> anyhow::Result<Arc<dyn PlatformTypesRegistryApi>> {
        Ok(Self::seeded(ctx)?)
    }

    /// `provides` factory: the tenant API over the same seeded database.
    fn seeded_tenant_api(
        ctx: &GearCtx,
        _policies: Arc<PolicyStack>,
    ) -> anyhow::Result<Arc<dyn TypesRegistryApi>> {
        Ok(Self::seeded(ctx)?)
    }

    /// Publish both contracts over `registry`, through the `provides` wiring.
    async fn publish_apis(
        &self,
        ctx: &GearCtx,
        registry: &Arc<RegistryService>,
    ) -> anyhow::Result<()> {
        let seeded = SeededClient(Arc::new(LocalClient::new(Arc::clone(registry))));
        held_while(&ctx.client_hub(), Arc::new(seeded), async {
            self.wire_platform_types_registry_api(ctx).await?;
            self.wire_types_registry_api(ctx).await
        })
        .await
    }

    /// Start the optional database-backed admission pipeline and admit `seeds`
    /// through it. A seed failure stops the pipeline and fails boot.
    async fn wire_admission(
        &self,
        db: &DBProvider<DbError>,
        registration_policy: RegistrationPolicy,
        cfg: TypesRegistryConfig,
        metrics: Arc<dyn AdmissionMetrics>,
        seeds: &SeedSet,
        cancel: &CancellationToken,
    ) -> anyhow::Result<()> {
        let seed_budget = cfg.worker.operation_timeout;
        let dispatch = Arc::new(OutboxDispatch::new());
        // Choose database adapters at the composition root.
        let stores: Arc<dyn Stores> = Arc::new(Repos);
        let registry = Arc::new(RegistryService::new(
            db.db(),
            stores,
            registration_policy,
            cfg,
            Arc::clone(&dispatch) as Arc<dyn OperationDispatch>,
            metrics,
        ));

        // The dispatch must be bound before anything submits: acceptance enqueues
        // inside its own transaction and refuses if there is nowhere to enqueue.
        let admission = crate::infra::outbox::start(db.db(), &registry, &dispatch).await?;
        info!(
            queue = crate::infra::outbox::QUEUE,
            table_prefix = OUTBOX_TABLE_PREFIX,
            "types_registry admission outbox running"
        );

        // The seed barrier: nothing database-backed is published before every
        // seed is stored. A failed startup keeps no worker running.
        match seed(&registry, seeds, seed_budget, cancel).await {
            Ok(summary) => info!(
                documents = summary.documents,
                up_to_date = summary.up_to_date,
                "types_registry seeded the database"
            ),
            Err(error) => {
                admission.stop().await;
                return Err(anyhow::anyhow!(
                    "types-registry database seeding failed: {error}"
                ));
            }
        }
        *self.outbox.lock() = Some(admission);

        self.registry
            .set(registry)
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        Ok(())
    }

    /// Await runtime cancellation, then drain the pipeline started in `init()`.
    pub(crate) async fn serve(
        self: Arc<Self>,
        cancel: CancellationToken,
        ready: ReadySignal,
    ) -> anyhow::Result<()> {
        ready.notify();
        cancel.cancelled().await;

        // Release the mutex before awaiting the drain.
        let admission = self.outbox.lock().take();
        if let Some(admission) = admission {
            info!("types_registry draining the admission outbox");
            admission.stop().await;
            info!("types_registry admission outbox stopped");
        }
        Ok(())
    }
}

#[async_trait]
impl Gear for TypesRegistryGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: TypesRegistryConfig = ctx.config_or_default()?;

        // Build admission instruments eagerly from ToolKit's configured provider.
        let metrics_prefix = cfg.metrics.effective_prefix(Self::MODULE_NAME);
        let metrics: Arc<dyn AdmissionMetrics> =
            crate::infra::metrics::default_adapter(&metrics_prefix);

        // Fail boot on invalid policy regions (SPEC §10.3); reuse the compiled
        // policy for T7 acceptance so validation and enforcement agree.
        let registration_policy = cfg.validate()?;
        debug!(
            regions = registration_policy.len(),
            allow_compatibility_force = cfg.allow_compatibility_force,
            batch_candidates = cfg.limits.batch_candidates,
            "Validated types_registry registration policy and limits"
        );

        debug!(
            "Loaded types_registry config: entity_id_fields={:?}, schema_id_fields={:?}, \
             local_client.cache.type_schemas={{capacity={}, ttl={:?}}}, \
             local_client.cache.instances={{capacity={}, ttl={:?}}}",
            cfg.entity_id_fields,
            cfg.schema_id_fields,
            cfg.local_client.cache.type_schemas.capacity,
            cfg.local_client.cache.type_schemas.ttl,
            cfg.local_client.cache.instances.capacity,
            cfg.local_client.cache.instances.ttl,
        );

        let gts_config = cfg.to_gts_config();
        let static_entities = cfg.entities.clone();
        let cfg_for_registry = cfg.clone();
        let type_schemas_cache_cfg = cfg.local_client.cache.type_schemas.to_cache_config();
        let instances_cache_cfg = cfg.local_client.cache.instances.to_cache_config();

        let repo = Arc::new(InMemoryGtsRepository::new(gts_config));
        let service = Arc::new(TypesRegistryService::new(repo, cfg));

        let inventory_type_schemas = all_inventory_type_schemas()
            .map_err(|e| anyhow::anyhow!("Failed to collect GTS Type Schemas: {e}"))?;
        let inventory_instances = all_inventory_instances()
            .map_err(|e| anyhow::anyhow!("Failed to collect GTS Instances: {e}"))?;
        let schema_count = inventory_type_schemas.len();
        let instance_count = inventory_instances.len();
        let mut inventory_entries = inventory_type_schemas;
        inventory_entries.extend(inventory_instances);
        info!(
            schema_count,
            instance_count,
            configured_count = static_entities.len(),
            "types_registry seed set"
        );
        // The same declarations seed the database path; until T38 the two catalogues
        // are independent and nothing mirrors later writes between them. Copied only
        // when a database is bound, before the legacy catalogue consumes them.
        let seeded = ctx.db().map(|db| {
            let documents: Vec<serde_json::Value> = inventory_entries
                .iter()
                .chain(&static_entities)
                .cloned()
                .collect();
            (db, documents)
        });
        let seed_results = service.register(inventory_entries);
        RegisterResult::ensure_all_ok(&seed_results)
            .map_err(|e| anyhow::anyhow!("Failed to register GTS inventory: {e}"))?;

        // Register static entities from config (before ready-mode validation)
        if !static_entities.is_empty() {
            let entity_count = static_entities.len();
            let results = service.register(static_entities);
            let summary = RegisterSummary::from_results(&results);

            if !summary.all_succeeded() {
                for result in &results {
                    if let RegisterResult::Err { gts_id, error } = result {
                        tracing::error!(
                            gts_id = gts_id.as_deref().unwrap_or("<unknown>"),
                            error = %error,
                            "Failed to register static GTS entity"
                        );
                    }
                }
                anyhow::bail!(
                    "types-registry: {}/{} static entities failed to register",
                    summary.failed,
                    summary.total()
                );
            }

            info!(
                count = entity_count,
                "Registered static GTS entities from config"
            );
        }

        self.service
            .set(service.clone())
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        // T7–T9's database path is optional for `no-db.yaml` / `--mock` deployments.
        // Without a DB, routes return canonical `503 Service Unavailable`; warn why.
        if let Some((db, seed_documents)) = seeded {
            let seeds = SeedSet::collect(seed_documents, &cfg_for_registry.entity_id_fields)?;
            self.wire_admission(
                &db,
                registration_policy,
                cfg_for_registry,
                metrics,
                &seeds,
                ctx.cancellation_token(),
            )
            .await?;
        } else {
            tracing::warn!(
                "types_registry has no database bound: POST /entities, GET /operations/{{id}} and \
                 GET /entities/{{key}} will report service unavailable. Bind one under \
                 gears.types-registry.database to enable admission."
            );
        }

        let local_client = Arc::new(TypesRegistryLocalClient::with_cache_configs(
            service,
            type_schemas_cache_cfg,
            instances_cache_cfg,
        ));
        self.local_client
            .set(local_client.clone())
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        let api: Arc<dyn TypesRegistryClient> = local_client;
        ctx.client_hub().register::<dyn TypesRegistryClient>(api);

        // Only past the seed barrier: a host without a database, or one whose seed
        // failed, publishes neither, and a resolution fails rather than falling back.
        if let Some(registry) = self.registry.get() {
            self.publish_apis(ctx, registry).await?;
            info!("types_registry published the platform and tenant APIs in ClientHub");
        }

        Ok(())
    }
}

#[async_trait]
impl SystemCapability for TypesRegistryGear {
    /// Validate and enter ready mode after all gears finish `init()` and register types.
    async fn post_init(&self, _sys: &toolkit::runtime::SystemContext) -> anyhow::Result<()> {
        info!("types_registry post_init: switching to ready mode");

        let service = self
            .service
            .get()
            .ok_or_else(|| anyhow::anyhow!("Service not initialized"))?
            .clone();

        service.switch_to_ready().map_err(|e| {
            if let Some(errors) = e.validation_errors() {
                for err in errors {
                    // Try to get the entity content for debugging
                    let entity_content = match service.get(&err.gts_id) {
                        Ok(entity) => serde_json::to_string_pretty(&entity.content)
                            .unwrap_or_else(|_| "Failed to serialize".to_owned()),
                        _ => "Entity not found or failed to retrieve".to_owned(),
                    };

                    tracing::error!(
                        gts_id = %err.gts_id,
                        message = %err.message,
                        entity_content = %entity_content,
                        "GTS validation error"
                    );
                }
            }
            anyhow::anyhow!("Failed to switch to ready mode: {e}")
        })?;

        // Drop pre-ready builds with possibly unresolved parents; subsequent
        // get_*/list_* calls rebuild against the final persistent store.
        if let Some(client) = self.local_client.get() {
            client.clear_caches();
        }

        info!("types_registry switched to ready mode successfully");
        Ok(())
    }
}

impl DatabaseCapability for TypesRegistryGear {
    /// Managed-state schema plus `ToolKit`-owned outbox migrations from
    /// `outbox_migrations_with_prefix("types_registry__outbox")`. Keeping outbox DDL
    /// out of the initial migration lets `ToolKit` evolve it without local drift.
    fn migrations(&self) -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        info!("Providing types-registry database migrations");
        let mut migrations = crate::infra::storage::Migrator::migrations();
        let outbox = match toolkit_db::outbox::outbox_migrations_with_prefix(OUTBOX_TABLE_PREFIX) {
            Ok(outbox) => outbox,
            // `migrations()` cannot return errors. Only an invalid constant prefix
            // (e.g. schema-qualified) reaches here; abort rather than boot without outbox tables.
            Err(e) => panic!(
                "types-registry outbox migration prefix '{OUTBOX_TABLE_PREFIX}' is invalid: {e}"
            ),
        };
        migrations.extend(outbox);
        migrations
    }
}

impl RestApiCapability for TypesRegistryGear {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        info!("Registering types_registry REST routes");

        let service = self
            .service
            .get()
            .ok_or_else(|| anyhow::anyhow!("Service not initialized"))?
            .clone();

        // Where no database is bound there is no `RegistryService`, and the
        // database-backed routes must still exist so a caller gets a problem
        // document naming the cause rather than a 404 suggesting the API changed.
        let registry = self.registry.get().cloned();
        let router = crate::api::rest::routes::register_routes(router, openapi, service, registry);

        info!("Types registry REST routes registered successfully");
        Ok(router)
    }
}

#[cfg(test)]
#[path = "gear_tests.rs"]
mod gear_tests;
