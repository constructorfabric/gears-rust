//! Gear declaration for the Types Registry gear.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
use toolkit::contracts::{DatabaseCapability, SystemCapability};
use toolkit::lifecycle::ReadySignal;
use toolkit::{Gear, GearCtx, RestApiCapability};
use toolkit_db::outbox::OutboxHandle;
use toolkit_gts::{all_inventory_instances, all_inventory_type_schemas};
use tracing::{debug, info, warn};
use types_registry_sdk::{RegisterResult, RegisterSummary, TypesRegistryClient};

use crate::config::TypesRegistryConfig;
use crate::domain::admission::OperationDispatch;
use crate::domain::local_client::TypesRegistryLocalClient;
use crate::domain::ports::Stores;
use crate::domain::ports::metrics::AdmissionMetrics;
use crate::domain::registry_service::RegistryService;
use crate::domain::service::TypesRegistryService;
use crate::infra::InMemoryGtsRepository;
use crate::infra::outbox::{OutboxDispatch, TABLE_PREFIX as OUTBOX_TABLE_PREFIX};
use crate::infra::storage::Repos;

/// Types Registry gear.
///
/// Provides GTS entity registration, storage, validation, and REST API endpoints.
///
/// ## Capabilities
///
/// - `system` — Core infrastructure gear, initialized early in startup
/// - `db` — Owns the managed-state schema (`docs/database.sql`, P0 subset)
/// - `rest` — Exposes REST API endpoints
/// - `stateful` — Owns the admission outbox worker's lifetime (T21)
///
/// ## Worker lifecycle
///
/// Start in `init()` so consumers can await registration during their own `init()`;
/// all gears initialize before any stateful `start` runs (plan P3).
/// `serve` awaits runtime cancellation and calls `OutboxHandle::stop()` to cancel
/// the outbox's internal token and join its workers.
///
/// ## Link-time inventory seeding
///
/// Seed all `toolkit-gts` inventory entries: `InventoryTypeSchema` / `InventoryInstance`
/// populated by linked crates' `#[gts_type_schema]` / `gts_instance!`. Call internal
/// `TypesRegistryService::register` before publishing the client (no `ClientHub`
/// round-trip), so consumers see base types on first access.
///
/// `all_inventory_type_schemas()` / `all_inventory_instances()` discover entries
/// from the dependency graph without naming specific types in types-registry.
#[toolkit::gear(
    name = "types-registry",
    capabilities = [system, db, rest, stateful],
    // 30s, matching `HostRuntime::DEFAULT_SHUTDOWN_DEADLINE`'s assumption that a
    // gear's lifecycle timeout fires 5s before the runtime's 35s hard backstop.
    // This does NOT cover the worst-case outbox lease (`worker.operation_timeout`
    // + 2s, 5 minutes by default): the processor ignores cancellation while a
    // leased handler runs, so a long admission outlives the drain and `serve` is
    // aborted. Declaring a larger value would not help — the host hard-stops at 35s
    // whatever the gear asks for.
    //
    // Aborting `serve` destroys the future awaiting the drain, not the spawned
    // outbox tasks, so a pass in flight keeps running with no bound on when it ends.
    // What that costs is bounded, not what it takes: an interrupted pass is
    // resumable (see the cancel-safety note on `AdmissionHandler::admit_payload`)
    // and recovery re-enqueues whatever stayed non-terminal on the next boot. Task
    // lifetime itself is a lifecycle guarantee this gear does not provide.
    lifecycle(entry = "serve", stop_timeout = "30s", await_ready)
)]
pub struct TypesRegistryGear {
    service: OnceLock<Arc<TypesRegistryService>>,
    /// The database-backed path. Absent when no database is bound to this gear.
    registry: OnceLock<Arc<RegistryService>>,
    local_client: OnceLock<Arc<TypesRegistryLocalClient>>,
    /// Pipeline retained from `init()` for shutdown; absent without a DB or after draining.
    outbox: tokio::sync::Mutex<Option<OutboxHandle>>,
}

impl Default for TypesRegistryGear {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
            registry: OnceLock::new(),
            local_client: OnceLock::new(),
            outbox: tokio::sync::Mutex::new(None),
        }
    }
}

impl TypesRegistryGear {
    /// Await runtime cancellation, then stop and join the pipeline started in `init()`.
    pub(crate) async fn serve(
        self: Arc<Self>,
        cancel: CancellationToken,
        ready: ReadySignal,
    ) -> anyhow::Result<()> {
        ready.notify();
        cancel.cancelled().await;

        // Taking the handle makes repeated shutdown a no-op.
        if let Some(handle) = self.outbox.lock().await.take() {
            info!("types_registry draining the admission outbox");
            handle.stop().await;
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

        // Warn once at boot so operators do not mistake accepted settings for enforced bounds.
        let inert = cfg.inert_limit_keys();
        if !inert.is_empty() {
            warn!(
                keys = ?inert,
                "types_registry accepted configuration keys that P0 does not enforce; \
                 see each key's documentation for its enforcement status"
            );
        }

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
        debug!(
            schema_count,
            instance_count, "Seeding GTS inventory into types-registry"
        );
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
        if let Some(db) = ctx.db() {
            // Bind after pipeline creation; the weak link breaks the ownership cycle.
            let dispatch = Arc::new(OutboxDispatch::new());
            // The domain names its persistence ports and never the repositories;
            // this is the one place the database-backed adapter is chosen.
            let stores: Arc<dyn Stores> = Arc::new(Repos);
            let registry = Arc::new(RegistryService::new(
                db.db(),
                stores,
                registration_policy,
                cfg_for_registry,
                Arc::clone(&dispatch) as Arc<dyn OperationDispatch>,
                // Dispatch production admissions; seeding uses a separate inline service (SPEC §8.1).
                crate::domain::registry_service::AdmissionMode::Outbox,
                Arc::clone(&metrics),
            ));

            // Start after inline seeding to avoid concurrent seed admission (plan P3).
            let handle = crate::infra::outbox::start(db.db(), &registry, &dispatch).await?;
            *self.outbox.lock().await = Some(handle);

            self.registry
                .set(registry)
                .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
            info!(
                queue = crate::infra::outbox::QUEUE,
                table_prefix = OUTBOX_TABLE_PREFIX,
                "types_registry database-backed admission path wired; outbox worker running"
            );
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
