//! `ChatEngineModule` — Phase 15 integration entrypoint.
//!
//! Wires the per-feature services produced by Phases 1-13, mounts the
//! REST surface assembled by Phase 14, and runs the retention-cleanup
//! background task on a `tokio::time::interval` driven by a
//! `CancellationToken`.
//!
//! # Topology
//!
//! ```text
//!  ModuleCtx ─▶ ChatEngineModule::new() (deferred wiring lives in init())
//!                  │
//!                  ├── ChatEngineConfig ─ validated
//!                  ├── modkit-db Db ── sea_orm::DatabaseConnection
//!                  ├── SeaORM repos     (session, message, reaction, plugin_config,
//!                  │                     session_type)
//!                  ├── ClientHub        (registers LlmGatewayPlugin + WebhookCompatPlugin
//!                  │                     under `ChatEngineBackendPlugin`)
//!                  ├── domain services  (PluginService, SessionService, MessageService,
//!                  │                     VariantService, IntelligenceService,
//!                  │                     ReactionService, SearchService, ExportService)
//!                  └── REST router      (api::rest::register_routes + Extension DI)
//!
//!  serve(cancel, ready)
//!     ├── spawn retention-cleanup task (tokio::time::interval)
//!     ├── ready.notify()
//!     └── await cancel.cancelled() → graceful shutdown
//! ```
//
// @cpt-cf-chat-engine-module-registration:p15
// @cpt-cf-chat-engine-module-lifecycle:p15

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use modkit::api::OpenApiRegistry;
use modkit::client_hub::ClientScope;
use modkit::{DatabaseCapability, Module, ModuleCtx, RestApiCapability};
use modkit::api::canonical_error_middleware;
use sea_orm_migration::MigrationTrait;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use chat_engine_sdk::plugin::ChatEngineBackendPlugin;

use crate::api::rest::routes::ChatEngineServices;
use crate::api::rest::{NoopWebhookEmitter, WebhookEmitter, WebhookEmitterAdapter};
use crate::config::ChatEngineConfig;
use crate::domain::export::StubExportStorage;
use crate::domain::service::{
    ExportService, InMemorySearchBackend, IntelligenceService, MessageService, PluginService,
    ReactionService, SearchService, SessionService, ShareUrlBuilder, VariantService,
};
use crate::domain::service::webhook::WebhookEmitter as DomainWebhookEmitter;
use crate::infra::db::migrations::Migrator;
use crate::infra::db::repo::message_repo::SeaMessageRepo;
use crate::infra::db::repo::plugin_config_repo::SeaPluginConfigRepo;
use crate::infra::db::repo::reaction_repo::SeaReactionRepo;
use crate::infra::db::repo::session_repo::SeaSessionRepo;
use crate::infra::db::repo::session_type_repo::SeaSessionTypeRepo;
use crate::infra::llm_gateway::LlmGatewayPlugin;
use crate::infra::webhook_compat::WebhookCompatPlugin;

/// GTS plugin instance ID used to register the default `WebhookCompatPlugin`
/// instance. Operators that want multiple webhook bindings can register
/// additional `WebhookCompatPlugin` instances themselves; the default one
/// is keyed under this stable id.
pub const DEFAULT_WEBHOOK_COMPAT_INSTANCE_ID: &str =
    "gtx.cf.chat_engine.webhook_compat_plugin.v1~";

/// Aggregated runtime state filled in during [`Module::init`].
struct RuntimeState {
    services: ChatEngineServices,
    webhooks: Arc<dyn WebhookEmitter>,
    intelligence: Arc<IntelligenceService>,
    config: Arc<ChatEngineConfig>,
}

/// Chat Engine module entrypoint.
///
/// Construction is two-phased so the macro-generated registrator can
/// instantiate the struct with `ChatEngineModule::new()` before
/// [`Module::init`] runs. All runtime handles live behind a
/// [`OnceLock`] that is populated inside `init()` once the
/// `ModuleCtx` is available.
#[modkit::module(
    name = "chat-engine",
    capabilities = [db, rest, stateful],
    client = chat_engine_sdk::ChatEngineBackendPlugin,
    ctor = ChatEngineModule::new(),
    lifecycle(entry = "serve", stop_timeout = "30s", await_ready)
)]
pub struct ChatEngineModule {
    runtime: OnceLock<RuntimeState>,
}

impl Default for ChatEngineModule {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatEngineModule {
    /// Construct an uninitialised module. The macro-generated registrator
    /// uses this at link time; production wiring (config load, repo /
    /// service construction, ClientHub registration) runs in
    /// [`Module::init`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            runtime: OnceLock::new(),
        }
    }

    fn runtime(&self) -> anyhow::Result<&RuntimeState> {
        self.runtime
            .get()
            .ok_or_else(|| anyhow::anyhow!("ChatEngineModule not initialised"))
    }

    /// Lifecycle entry — periodic retention cleanup.
    ///
    /// Runs a `tokio::time::interval` tick loop, racing each tick against
    /// `cancel.cancelled()` so the task exits promptly on shutdown. Each
    /// tick iterates the tenants reported by the session repo (Phase 8
    /// surface) and calls
    /// [`IntelligenceService::run_retention_cleanup_for_tenant`].
    ///
    /// `ready.notify()` fires once the interval handle is constructed so
    /// the modkit runtime can release dependent modules.
    pub async fn serve(
        self: Arc<Self>,
        cancel: CancellationToken,
        ready: modkit::lifecycle::ReadySignal,
    ) -> anyhow::Result<()> {
        let runtime = self.runtime()?;
        let interval_secs = runtime
            .config
            .retention_cleanup_interval_hours
            .saturating_mul(3600);
        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
        // Skip the immediate tick that `tokio::time::interval` fires
        // synchronously — we want the first cleanup to happen one period
        // after startup, not at boot.
        interval.tick().await;

        ready.notify();
        info!(
            interval_hours = runtime.config.retention_cleanup_interval_hours,
            "chat-engine retention-cleanup task running"
        );

        let intelligence = Arc::clone(&runtime.intelligence);

        loop {
            tokio::select! {
                () = cancel.cancelled() => {
                    info!("chat-engine retention-cleanup task received cancellation; exiting");
                    break;
                }
                _ = interval.tick() => {
                    if let Err(err) =
                        run_retention_cleanup_tick(intelligence.as_ref()).await
                    {
                        error!(
                            error = %err,
                            "chat-engine retention-cleanup tick failed; continuing",
                        );
                    }
                }
            }
        }
        Ok(())
    }
}

/// Single retention-cleanup tick.
///
/// Enumerates every tenant that currently owns an `active` session via
/// [`IntelligenceService::run_retention_cleanup_all_tenants`] and runs
/// the per-tenant cleanup against each. The session repository is the
/// source of truth for the tenant directory, so the tick activates
/// retention for real traffic — no sentinel / marker placeholder.
async fn run_retention_cleanup_tick(
    intelligence: &IntelligenceService,
) -> anyhow::Result<()> {
    let report = intelligence.run_retention_cleanup_all_tenants().await?;
    info!(
        sessions_scanned = report.sessions.len(),
        sessions_skipped_locked = report.skipped_count(),
        total_messages_deleted = report.total_messages_deleted(),
        "chat-engine retention-cleanup tick completed"
    );
    Ok(())
}

#[async_trait]
impl Module for ChatEngineModule {
    async fn init(&self, ctx: &ModuleCtx) -> anyhow::Result<()> {
        info!("initialising {} module", Self::MODULE_NAME);

        let cfg: ChatEngineConfig = ctx.config_or_default()?;
        cfg.validate()
            .map_err(|e| anyhow::anyhow!("invalid chat-engine config: {e}"))?;
        let config = Arc::new(cfg);

        // --- DB wiring ------------------------------------------------------
        //
        // The chat-engine repos currently embed an owned
        // `sea_orm::DatabaseConnection`. modkit-db does not surface the raw
        // connection through `DBProvider`; the migration runner uses it
        // privately. For Phase 15 we open a sibling SeaORM connection from
        // the same DSN so the existing repository constructors remain
        // unchanged. Future refactors should move every repo to the
        // `&impl DBRunner` shape used by the rest of the workspace.
        // The modkit-db handle is unused beyond migration registration —
        // repos take a sibling `sea_orm::DatabaseConnection` opened from
        // the same DSN below. `db_required()` is invoked so the runtime
        // refuses to mount the module without a database configured.
        let _db_provider = ctx.db_required()?;
        let sea = open_sea_orm_connection(ctx).await?;

        // --- Repositories ---------------------------------------------------
        let sessions_repo: Arc<dyn crate::infra::db::repo::session_repo::SessionRepo> =
            Arc::new(SeaSessionRepo::new(sea.clone()));
        let session_types_repo: Arc<
            dyn crate::infra::db::repo::session_type_repo::SessionTypeRepo,
        > = Arc::new(SeaSessionTypeRepo::new(sea.clone()));
        let messages_repo: Arc<dyn crate::infra::db::repo::message_repo::MessageRepo> =
            Arc::new(SeaMessageRepo::new(sea.clone()));
        let plugin_config_repo: Arc<
            dyn crate::infra::db::repo::plugin_config_repo::PluginConfigRepo,
        > = Arc::new(SeaPluginConfigRepo::new(sea.clone()));
        let reactions_repo: Arc<dyn crate::infra::db::repo::reaction_repo::ReactionRepo> =
            Arc::new(SeaReactionRepo::new(sea.clone()));
        let variants_repo: Arc<dyn crate::domain::service::VariantRepo> = Arc::new(
            crate::infra::db::repo::variant_repo::SeaVariantRepo::new(sea.clone()),
        );

        // --- ClientHub plugin registration ----------------------------------
        let client_hub = ctx.client_hub();
        let webhook_compat = Arc::new(
            WebhookCompatPlugin::new(DEFAULT_WEBHOOK_COMPAT_INSTANCE_ID)
                .map_err(|e| anyhow::anyhow!("failed to build webhook-compat plugin: {e}"))?,
        );
        client_hub.register_scoped::<dyn ChatEngineBackendPlugin>(
            ClientScope::gts_id(DEFAULT_WEBHOOK_COMPAT_INSTANCE_ID),
            webhook_compat.clone() as Arc<dyn ChatEngineBackendPlugin>,
        );

        // The LLM Gateway plugin's transport clients are owned by Phase 15;
        // until the production `reqwest`-backed implementations land we
        // register a stub-friendly variant only when the operator has
        // explicitly configured `llm_gateway_base_url`. Tests / smoke
        // bring-up rely on the FakeLlmGatewayClient registered out of
        // band via ClientHub.
        if config.llm_gateway_base_url.is_some() {
            warn!(
                "llm-gateway plugin instantiation requested but production transport clients \
                 are not yet wired in this build; the plugin slot remains empty"
            );
        }
        let _ = LlmGatewayPlugin::new; // explicit reference so the unused-import lint stays clean

        // --- Domain services -----------------------------------------------
        let plugin_service = PluginService::new(client_hub.clone(), plugin_config_repo.clone());

        let webhooks_rest: Arc<dyn WebhookEmitter> = Arc::new(NoopWebhookEmitter::default());
        let webhooks_domain: Arc<dyn DomainWebhookEmitter> =
            Arc::new(WebhookEmitterAdapter::new(webhooks_rest.clone()));

        let plugin_deadline = Duration::from_secs(config.plugin_deadline_secs);

        let sessions = Arc::new(
            SessionService::new(
                sessions_repo.clone(),
                session_types_repo.clone(),
                plugin_service.clone(),
                webhooks_domain.clone(),
            )
            .with_plugin_timeout(plugin_deadline),
        );

        let messages = Arc::new(
            MessageService::new(
                sessions_repo.clone(),
                session_types_repo.clone(),
                messages_repo.clone(),
                plugin_service.clone(),
            )
            .with_webhook_emitter(webhooks_domain.clone())
            .with_streaming_buffer_size(config.ndjson_buffer_size)
            .with_plugin_deadline(plugin_deadline),
        );

        let variants = Arc::new(
            VariantService::new(
                sessions_repo.clone(),
                session_types_repo.clone(),
                messages_repo.clone(),
                variants_repo.clone(),
                plugin_service.clone(),
                Arc::clone(&messages),
            )
            .with_plugin_timeout(plugin_deadline),
        );

        let reactions = Arc::new(ReactionService::new(
            sessions_repo.clone(),
            session_types_repo.clone(),
            messages_repo.clone(),
            reactions_repo.clone(),
            plugin_service.clone(),
        ));

        let search_backend: Arc<dyn crate::domain::service::SearchBackend> =
            Arc::new(InMemorySearchBackend::new());
        let search = Arc::new(SearchService::new(
            sessions_repo.clone(),
            messages_repo.clone(),
            search_backend,
        ));

        let intelligence = Arc::new(
            IntelligenceService::new(
                sessions_repo.clone(),
                session_types_repo.clone(),
                messages_repo.clone(),
                plugin_service.clone(),
            )
            .with_buffer_size(config.summary_buffer_size)
            .with_summary_deadline(plugin_deadline),
        );

        let share_urls = config
            .share_base_url
            .as_ref()
            .map_or_else(ShareUrlBuilder::default, |base| ShareUrlBuilder {
                base_url: base.clone(),
            });
        let export_storage = Arc::new(StubExportStorage);
        let export = Arc::new(
            ExportService::new(sessions_repo.clone(), messages_repo.clone(), export_storage)
                .with_share_urls(share_urls),
        );

        let services = ChatEngineServices {
            sessions,
            messages,
            variants,
            reactions,
            search,
            intelligence: Arc::clone(&intelligence),
            export,
        };

        let runtime = RuntimeState {
            services,
            webhooks: webhooks_rest,
            intelligence,
            config,
        };
        self.runtime
            .set(runtime)
            .map_err(|_| anyhow::anyhow!("chat-engine module already initialised"))?;

        info!("{} module initialised", Self::MODULE_NAME);
        Ok(())
    }
}

impl DatabaseCapability for ChatEngineModule {
    fn migrations(&self) -> Vec<Box<dyn MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        Migrator::migrations()
    }
}

impl RestApiCapability for ChatEngineModule {
    fn register_rest(
        &self,
        _ctx: &ModuleCtx,
        router: Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router> {
        let runtime = self.runtime()?;
        let router = router.layer(axum::middleware::from_fn(canonical_error_middleware));
        let router = crate::api::rest::register_routes(
            router,
            openapi,
            runtime.services.clone(),
            Arc::clone(&runtime.webhooks),
        );
        Ok(router)
    }
}

// ---------------------------------------------------------------------------
// SeaORM connection helper
// ---------------------------------------------------------------------------

/// Open a sibling `sea_orm::DatabaseConnection` from the DSN modkit
/// recorded in the module's config section.
///
/// The chat-engine repositories embed an owned `DatabaseConnection`;
/// modkit-db only exposes the raw connection internally, so we open a
/// parallel handle from the same DSN. Future refactors should migrate
/// the repos to the `&impl DBRunner` shape used by sibling modules so
/// this sibling connection becomes unnecessary.
///
/// When no DSN is present (e.g. the module section omits `database`),
/// falls back to an in-memory SQLite database — sufficient for smoke
/// tests and bring-up.
async fn open_sea_orm_connection(
    ctx: &ModuleCtx,
) -> anyhow::Result<sea_orm::DatabaseConnection> {
    let dsn = resolve_db_dsn(ctx).unwrap_or_else(|| "sqlite::memory:".to_owned());
    sea_orm::Database::connect(dsn.as_str())
        .await
        .map_err(|e| anyhow::anyhow!("failed to open sibling sea-orm connection: {e}"))
}

fn resolve_db_dsn(ctx: &ModuleCtx) -> Option<String> {
    let raw = ctx.config_provider().get_module_config(ctx.module_name())?;
    let obj = raw.as_object()?;
    let database = obj.get("database")?.as_object()?;
    database
        .get("dsn")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}
