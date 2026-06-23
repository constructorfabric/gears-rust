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
use sea_orm_migration::MigrationTrait;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
use toolkit::api::canonical_error_middleware;
use toolkit::client_hub::ClientScope;
use toolkit::{DatabaseCapability, Gear, GearCtx, RestApiCapability};
use toolkit_db::DBProvider;
use tracing::{error, info, warn};

use crate::infra::db::repo::ChatEngineDb;

use chat_engine_sdk::plugin::ChatEngineBackendPlugin;

use crate::api::rest::routes::ChatEngineServices;
use crate::api::rest::{NoopWebhookEmitter, WebhookEmitter, WebhookEmitterAdapter};
use crate::config::ChatEngineConfig;
use crate::domain::export::NotImplementedExportStorage;
use crate::domain::service::webhook::WebhookEmitter as DomainWebhookEmitter;
use crate::domain::service::{
    ExportService, IntelligenceService, MessageService, PluginService, ReactionService,
    SearchService, SessionService, ShareUrlBuilder, VariantService,
};
use crate::infra::db::migrations::Migrator;
use crate::infra::db::repo::message_repo::SeaMessageRepo;
use crate::infra::db::repo::plugin_config_repo::SeaPluginConfigRepo;
use crate::infra::db::repo::reaction_repo::SeaReactionRepo;
use crate::infra::db::repo::session_repo::SeaSessionRepo;
use crate::infra::db::repo::session_type_repo::SeaSessionTypeRepo;
use crate::infra::leader::{LeaderElector, work_fn};
use crate::infra::llm_gateway::LlmGatewayPlugin;
use crate::infra::search::NotImplementedSearchBackend;
use crate::infra::webhook_compat::WebhookCompatPlugin;

/// GTS plugin instance ID used to register the default `WebhookCompatPlugin`
/// instance. Operators that want multiple webhook bindings can register
/// additional `WebhookCompatPlugin` instances themselves; the default one
/// is keyed under this stable id.
pub const DEFAULT_WEBHOOK_COMPAT_INSTANCE_ID: &str = "gtx.cf.chat_engine.webhook_compat_plugin.v1~";

/// Aggregated runtime state filled in during [`Gear::init`].
struct RuntimeState {
    services: ChatEngineServices,
    webhooks: Arc<dyn WebhookEmitter>,
    intelligence: Arc<IntelligenceService>,
    /// Resume buffer (FR-024) shared between the streaming driver (writer) and
    /// the `Last-Event-ID` reconnect handler (reader); also swept by the TTL
    /// cleanup loop.
    stream_buffer: Arc<dyn crate::infra::db::repo::stream_event_repo::StreamEventBuffer>,
    config: Arc<ChatEngineConfig>,
    /// Leader elector gating the retention-cleanup loop so only one replica
    /// sweeps at a time under horizontal scaling
    /// (`@cpt-cf-chat-engine-adr-stateless-scaling`). Single-process / non-k8s
    /// builds use the noop elector (always leader).
    leader: Arc<dyn LeaderElector>,
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

    /// Lifecycle entry — periodic retention cleanup, gated by leader election.
    ///
    /// The per-tenant sweep is wrapped in
    /// [`LeaderElector::run_role`](crate::infra::leader::LeaderElector::run_role)
    /// so that under horizontal scaling
    /// (`@cpt-cf-chat-engine-adr-stateless-scaling`) only the lease holder
    /// runs it — every replica running the full sweep concurrently would
    /// duplicate work, and the per-tenant advisory lock the cleanup relies on
    /// is a no-op on the SQLite path. Single-process / non-k8s builds use the
    /// noop elector (always leader), preserving the original behaviour.
    ///
    /// `ready.notify()` fires before entering `run_role`: readiness gates
    /// dependent gears, not leadership, so a follower replica that never wins
    /// the lease is still "ready". On lease loss the work token is cancelled
    /// mid-loop and the elector re-campaigns; `cancel` (gear shutdown) tears
    /// the whole thing down.
    pub async fn serve(
        self: Arc<Self>,
        cancel: CancellationToken,
        ready: toolkit::lifecycle::ReadySignal,
    ) -> anyhow::Result<()> {
        let runtime = self.runtime()?;
        let interval_hours = runtime.config.retention_cleanup_interval_hours;
        let period = Duration::from_secs(interval_hours.saturating_mul(3600));
        let intelligence = Arc::clone(&runtime.intelligence);
        let leader = Arc::clone(&runtime.leader);

        ready.notify();
        info!(
            interval_hours,
            "chat-engine retention-cleanup task running (leader-gated)"
        );

        leader
            .run_role(
                "retention-cleanup",
                cancel,
                work_fn(move |cancel| {
                    let intelligence = Arc::clone(&intelligence);
                    async move {
                        let mut interval = tokio::time::interval(period);
                        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                        // Skip the immediate tick that `tokio::time::interval`
                        // fires synchronously — the first cleanup runs one
                        // period after acquiring leadership, not instantly.
                        interval.tick().await;

                        loop {
                            tokio::select! {
                                () = cancel.cancelled() => {
                                    info!(
                                        "chat-engine retention-cleanup loop stopping \
                                         (leadership lost or shutdown)"
                                    );
                                    return Ok(());
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
                    }
                }),
            )
            .await
    }
}

/// Single retention-cleanup tick.
///
/// Enumerates every tenant that currently owns an `active` session via
/// [`IntelligenceService::run_retention_cleanup_all_tenants`] and runs
/// the per-tenant cleanup against each. The session repository is the
/// source of truth for the tenant directory, so the tick activates
/// retention for real traffic — no sentinel / marker placeholder.
async fn run_retention_cleanup_tick(intelligence: &IntelligenceService) -> anyhow::Result<()> {
    let report = intelligence.run_retention_cleanup_all_tenants().await?;
    info!(
        sessions_scanned = report.sessions.len(),
        sessions_skipped_locked = report.skipped_count(),
        total_messages_deleted = report.total_messages_deleted(),
        "chat-engine retention-cleanup tick completed"
    );
    Ok(())
}

/// Construct the leader elector that gates the retention-cleanup loop.
///
/// With the `k8s` feature, uses a `coordination.k8s.io/v1` Lease keyed under
/// `chat-engine-{role}` (requires `POD_NAMESPACE` / `POD_NAME` + kube client
/// access). Otherwise a noop elector that is always leader — correct for
/// single-process / on-prem deployments. Mirrors mini-chat's
/// `background_workers::create_leader_elector`.
#[allow(
    clippy::unused_async,
    reason = "async is needed when the k8s feature is enabled"
)]
async fn create_leader_elector() -> anyhow::Result<Arc<dyn LeaderElector>> {
    #[cfg(feature = "k8s")]
    {
        use crate::infra::leader::k8s_lease::{K8sLeaseConfig, K8sLeaseElector};
        use anyhow::Context as _;

        let config = K8sLeaseConfig::from_env("chat-engine")
            .context("k8s feature enabled: POD_NAMESPACE and POD_NAME are required")?;
        let elector = K8sLeaseElector::from_default(config)
            .await
            .context("k8s feature enabled: kube client init failed")?;
        info!("chat-engine: using k8s Lease leader election");
        Ok(Arc::new(elector))
    }
    #[cfg(not(feature = "k8s"))]
    {
        info!("chat-engine: using noop leader election (single-process mode)");
        Ok(crate::infra::leader::noop())
    }
}

#[async_trait]
impl Gear for ChatEngineModule {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        info!("initialising {} module", Self::MODULE_NAME);

        let cfg: ChatEngineConfig = ctx.config_or_default()?;
        cfg.validate()
            .map_err(|e| anyhow::anyhow!("invalid chat-engine config: {e}"))?;
        let config = Arc::new(cfg);

        // Leader elector gating the retention-cleanup loop (one sweeper per
        // cluster under horizontal scaling). Constructed up front so a
        // misconfigured k8s runtime fails init() rather than the serve loop.
        let leader = create_leader_elector().await?;

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

        // Resume buffer (FR-024): the driver tees wire events here so dropped
        // connections can resume via `Last-Event-ID`.
        let stream_buffer: Arc<dyn crate::infra::db::repo::stream_event_repo::StreamEventBuffer> =
            Arc::new(crate::infra::db::repo::stream_event_repo::SeaStreamEventBuffer::new(
                Arc::clone(&db),
            ));

        let messages = Arc::new(
            MessageService::new(
                sessions_repo.clone(),
                session_types_repo.clone(),
                messages_repo.clone(),
                plugin_service.clone(),
            )
            .with_webhook_emitter(webhooks_domain.clone())
            .with_streaming_buffer_size(config.ndjson_buffer_size)
            .with_plugin_deadline(plugin_deadline)
            .with_stream_buffer(Arc::clone(&stream_buffer)),
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

        let share_urls =
            config
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
            stream_buffer,
            config,
            leader,
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
            Arc::clone(&runtime.stream_buffer),
            runtime.config.enable_search,
        );
        Ok(router)
    }
}
