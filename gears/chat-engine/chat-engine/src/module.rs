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
//!  GearCtx ─▶ ChatEngineModule::new() (deferred wiring lives in init())
//!                  │
//!                  ├── ChatEngineConfig ─ validated
//!                  ├── toolkit-db Db ── sea_orm::DatabaseConnection
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
use toolkit::api::OpenApiRegistry;
use toolkit::client_hub::ClientScope;
use toolkit::{DatabaseCapability, Gear, GearCtx, RestApiCapability};
use toolkit::api::canonical_error_middleware;
use toolkit_db::DBProvider;
use sea_orm_migration::MigrationTrait;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::infra::db::repo::ChatEngineDb;

use chat_engine_sdk::plugin::ChatEngineBackendPlugin;

use crate::api::rest::routes::ChatEngineServices;
use crate::api::rest::{NoopWebhookEmitter, WebhookEmitter, WebhookEmitterAdapter};
use crate::config::ChatEngineConfig;
use crate::domain::export::NotImplementedExportStorage;
use crate::domain::service::{
    ExportService, IntelligenceService, MessageService, PluginService, ReactionService,
    SearchService, SessionService, ShareUrlBuilder, VariantService,
};
use crate::infra::search::NotImplementedSearchBackend;
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

/// Aggregated runtime state filled in during [`Gear::init`].
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
/// [`Gear::init`] runs. All runtime handles live behind a
/// [`OnceLock`] that is populated inside `init()` once the
/// `GearCtx` is available.
#[toolkit::gear(
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
    /// [`Gear::init`].
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
    /// the toolkit runtime can release dependent modules.
    pub async fn serve(
        self: Arc<Self>,
        cancel: CancellationToken,
        ready: toolkit::lifecycle::ReadySignal,
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
impl Gear for ChatEngineModule {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        info!("initialising {} module", Self::MODULE_NAME);

        let cfg: ChatEngineConfig = ctx.config_or_default()?;
        cfg.validate()
            .map_err(|e| anyhow::anyhow!("invalid chat-engine config: {e}"))?;
        let config = Arc::new(cfg);

        // --- DB wiring ------------------------------------------------------
        //
        // Thread the toolkit-db `DBProvider` returned by `ctx.db_required()`
        // straight into every repo so reads/writes land on the same handle
        // the migration runner used. Earlier revisions opened a sibling
        // `sea_orm::DatabaseConnection` from a private `database.dsn`
        // config key — that path silently fell back to in-memory SQLite
        // when the key was absent and bypassed toolkit-db's pool sizing,
        // observability, and SecureConn enforcement. The provider is
        // reparameterised over `ChatEngineError` so `?` lifts both
        // `DbError` and `ScopeError` into the crate's domain enum.
        let db_raw = ctx.db_required()?;
        let db: Arc<ChatEngineDb> = Arc::new(DBProvider::new(db_raw.db()));

        // --- Repositories ---------------------------------------------------
        let sessions_repo: Arc<dyn crate::infra::db::repo::session_repo::SessionRepo> =
            Arc::new(SeaSessionRepo::new(Arc::clone(&db)));
        let session_types_repo: Arc<
            dyn crate::infra::db::repo::session_type_repo::SessionTypeRepo,
        > = Arc::new(SeaSessionTypeRepo::new(Arc::clone(&db)));
        let messages_repo: Arc<dyn crate::infra::db::repo::message_repo::MessageRepo> =
            Arc::new(SeaMessageRepo::new(Arc::clone(&db)));
        let plugin_config_repo: Arc<
            dyn crate::infra::db::repo::plugin_config_repo::PluginConfigRepo,
        > = Arc::new(SeaPluginConfigRepo::new(Arc::clone(&db)));
        let reactions_repo: Arc<dyn crate::infra::db::repo::reaction_repo::ReactionRepo> =
            Arc::new(SeaReactionRepo::new(Arc::clone(&db)));
        let variants_repo: Arc<dyn crate::domain::service::VariantRepo> = Arc::new(
            crate::infra::db::repo::variant_repo::SeaVariantRepo::new(Arc::clone(&db)),
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

        // Production wiring intentionally uses the not-implemented backend:
        // the `tsvector` / `LIKE` backends are not yet wired, so enabling
        // search must surface an honest 501 rather than the in-memory
        // backend's silent empty result set (RUST-NO-001). Swap for a real
        // backend here once it lands.
        let search_backend: Arc<dyn crate::domain::service::SearchBackend> =
            Arc::new(NotImplementedSearchBackend::new());
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
            .with_summary_deadline(plugin_deadline)
            .with_retention_caps(
                config.retention_max_sessions_per_tick,
                config.retention_max_deletes_per_session,
            ),
        );

        let share_urls = config
            .share_base_url
            .as_ref()
            .map_or_else(ShareUrlBuilder::default, |base| ShareUrlBuilder {
                base_url: base.clone(),
            });
        // Not-implemented until a real object-storage backend is wired:
        // returns 501 rather than discarding the bytes behind a dead
        // `memory://` URL (RUST-NO-001).
        let export_storage = Arc::new(NotImplementedExportStorage);
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
        _ctx: &GearCtx,
        router: Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router> {
        let runtime = self.runtime()?;
        let router = router.layer(axum::middleware::from_fn(canonical_error_middleware));
        if !runtime.config.enable_search {
            info!(
                "chat-engine search endpoints disabled (enable_search=false); \
                 production search backends are still stubs",
            );
        }
        let router = crate::api::rest::register_routes(
            router,
            openapi,
            runtime.services.clone(),
            Arc::clone(&runtime.webhooks),
            runtime.config.enable_search,
        );
        Ok(router)
    }
}

