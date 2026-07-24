use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use authn_resolver_sdk::{AuthNResolverClient, ClientCredentialsRequest};
use authz_resolver_sdk::AuthZResolverClient;
use std::time::Duration;
use toolkit::api::OpenApiRegistry;
use toolkit::contracts::RunnableCapability;
use toolkit::{DatabaseCapability, Gear, GearCtx, RestApiCapability};

use oagw_sdk::ServiceGatewayClientV1;
use sea_orm_migration::MigrationTrait;
use tokio_util::sync::CancellationToken;
use toolkit_db::outbox::{LeaseConfig, Outbox, OutboxHandle, Partitions};
use tracing::info;

use crate::api::rest::routes;
use crate::background_workers::{self, WORKER_STOP_TIMEOUT, WorkerConfigs};
use crate::config::ProviderEntry;
use crate::domain::ports::MiniChatMetricsPort;
use crate::domain::service::{AppServices as GenericAppServices, Repositories};
use crate::infra::metrics::MiniChatMetricsMeter;
use crate::infra::outbox::{AuditEventHandler, InfraOutboxEnqueuer, UsageEventHandler};
use crate::infra::workers::WorkerHandles;

pub(crate) type AppServices = GenericAppServices<
    TurnRepository,
    MessageRepository,
    QuotaUsageRepository,
    ReactionRepository,
    ChatRepository,
    ThreadSummaryRepository,
    AttachmentRepository,
    VectorStoreRepository,
    MessageAttachmentRepository,
>;
use crate::infra::audit_gateway::AuditGateway;
use crate::infra::db::repo::attachment_repo::AttachmentRepository;
use crate::infra::db::repo::chat_repo::ChatRepository;
use crate::infra::db::repo::message_attachment_repo::MessageAttachmentRepository;
use crate::infra::db::repo::message_repo::MessageRepository;
use crate::infra::db::repo::quota_usage_repo::QuotaUsageRepository;
use crate::infra::db::repo::reaction_repo::ReactionRepository;
use crate::infra::db::repo::thread_summary_repo::ThreadSummaryRepository;
use crate::infra::db::repo::turn_repo::TurnRepository;
use crate::infra::db::repo::vector_store_repo::VectorStoreRepository;
use crate::infra::llm::provider_resolver::ProviderResolver;
use crate::infra::model_policy::ModelPolicyGateway;

/// Default URL prefix for all mini-chat REST routes.
pub const DEFAULT_URL_PREFIX: &str = "/mini-chat";

/// The mini-chat gear: multi-tenant AI chat with SSE streaming.
#[toolkit::gear(
    name = "mini-chat",
    deps = [types_registry, authn_resolver, authz_resolver, oagw],
    capabilities = [db, rest, stateful],
)]
pub struct MiniChatGear {
    service: OnceLock<Arc<AppServices>>,
    url_prefix: OnceLock<String>,
    outbox_handle: Mutex<Option<OutboxHandle>>,
    /// OAGW gateway + provider config for deferred upstream registration in `start()`.
    oagw_deferred: OnceLock<OagwDeferred>,
    /// Worker configs captured in `init()`, consumed by `start()`.
    worker_configs: OnceLock<WorkerConfigs>,
    worker_cancel: Mutex<Option<CancellationToken>>,
    /// Handles to spawned background workers — joined during `stop()`.
    worker_handles: Mutex<Option<WorkerHandles>>,
    /// Deferred outbox pipeline params — built in `init()`, started in `start()`.
    outbox_deferred: OnceLock<OutboxDeferred>,
}

/// State needed to register OAGW upstreams in `start()` (after GTS is ready).
struct OagwDeferred {
    gateway: Arc<dyn ServiceGatewayClientV1>,
    authn: Arc<dyn AuthNResolverClient>,
    client_credentials: crate::config::ClientCredentialsConfig,
    providers: std::collections::HashMap<String, ProviderEntry>,
}

/// State needed to build + start the outbox pipeline in `start()`.
/// Captured in `init()`, consumed in `start()` after OAGW registration.
struct OutboxDeferred {
    db: Arc<crate::domain::service::DbProvider>,
    outbox_config: crate::config::OutboxConfig,
    cleanup_config: crate::config::background::CleanupWorkerConfig,
    model_policy_gw: Arc<ModelPolicyGateway>,
    audit_gateway: Arc<AuditGateway>,
    file_storage: Arc<dyn crate::domain::ports::FileStorageProvider>,
    vector_store_prov: Arc<dyn crate::domain::ports::VectorStoreProvider>,
    metrics: Arc<dyn MiniChatMetricsPort>,
    enqueuer: Arc<InfraOutboxEnqueuer>,
    provider_resolver: Arc<crate::infra::llm::provider_resolver::ProviderResolver>,
    model_resolver: Arc<dyn crate::domain::repos::ModelResolver>,
    thread_summary_config: crate::config::background::ThreadSummaryWorkerConfig,
    /// `Some` when at least one configured provider uses the
    /// `anthropic_messages` adapter. Cleanup handlers use it for the
    /// secondary `DELETE /v1/files/{id}` after the primary delete succeeds.
    anthropic_files_client:
        Option<Arc<crate::infra::llm::providers::anthropic_files_client::AnthropicFilesClient>>,
}

impl Default for MiniChatGear {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
            url_prefix: OnceLock::new(),
            outbox_handle: Mutex::new(None),
            oagw_deferred: OnceLock::new(),
            worker_configs: OnceLock::new(),
            worker_cancel: Mutex::new(None),
            worker_handles: Mutex::new(None),
            outbox_deferred: OnceLock::new(),
        }
    }
}

#[allow(clippy::too_many_lines)]
#[async_trait]
impl Gear for MiniChatGear {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        info!("Initializing {} gear", Self::MODULE_NAME);

        let mut cfg: crate::config::MiniChatConfig = ctx.config_expanded_or_default()?;
        cfg.streaming
            .validate()
            .map_err(|e| anyhow::anyhow!("streaming config: {e}"))?;
        cfg.estimation_budgets
            .validate()
            .map_err(|e| anyhow::anyhow!("estimation_budgets config: {e}"))?;
        cfg.quota
            .validate()
            .map_err(|e| anyhow::anyhow!("quota config: {e}"))?;
        cfg.outbox
            .validate()
            .map_err(|e| anyhow::anyhow!("outbox config: {e}"))?;
        cfg.context
            .validate()
            .map_err(|e| anyhow::anyhow!("context config: {e}"))?;
        cfg.client_credentials
            .validate()
            .map_err(|e| anyhow::anyhow!("client_credentials config: {e}"))?;
        for (id, entry) in &cfg.providers {
            entry
                .validate(id)
                .map_err(|e| anyhow::anyhow!("providers config: {e}"))?;
        }
        cfg.validate_provider_refs()
            .map_err(|e| anyhow::anyhow!("providers config: {e}"))?;
        cfg.orphan_watchdog
            .validate()
            .map_err(|e| anyhow::anyhow!("orphan_watchdog config: {e}"))?;
        cfg.thread_summary_worker
            .validate()
            .map_err(|e| anyhow::anyhow!("thread_summary_worker config: {e}"))?;
        cfg.cleanup_worker
            .validate()
            .map_err(|e| anyhow::anyhow!("cleanup_worker config: {e}"))?;
        cfg.thumbnail
            .validate()
            .map_err(|e| anyhow::anyhow!("thumbnail config: {e}"))?;
        cfg.rag
            .validate()
            .map_err(|e| anyhow::anyhow!("rag config: {e}"))?;
        cfg.knowledge_search
            .validate()
            .map_err(|e| anyhow::anyhow!("knowledge_search config: {e}"))?;

        let vendor = cfg.vendor.trim().to_owned();
        if vendor.is_empty() {
            return Err(anyhow::anyhow!(
                "{}: vendor must be a non-empty string",
                Self::MODULE_NAME
            ));
        }

        // `MiniChatModelPolicyPluginSpecV1` and `MiniChatAuditPluginSpecV1`
        // schemas reach `types-registry` automatically through the
        // `toolkit-gts` link-time inventory. No per-gear schema registration
        // is needed here.

        self.url_prefix
            .set(cfg.url_prefix)
            .map_err(|_| anyhow::anyhow!("{} url_prefix already set", Self::MODULE_NAME))?;

        let db_provider = ctx.db_required()?;
        let db = Arc::new(db_provider);

        // Create the model-policy gateway early for both outbox handler and services.
        let model_policy_gw = Arc::new(ModelPolicyGateway::new(ctx.client_hub(), vendor.clone()));

        // Audit gateway: lazily resolves audit plugin(s) on first emission.
        let audit_gateway = Arc::new(AuditGateway::new(ctx.client_hub(), vendor));

        // ── Resolve infrastructure deps needed by both outbox handlers and services ──

        let authz = ctx
            .client_hub()
            .get::<dyn AuthZResolverClient>()
            .map_err(|e| anyhow::anyhow!("failed to get AuthZ resolver: {e}"))?;

        let authn_client = ctx
            .client_hub()
            .get::<dyn AuthNResolverClient>()
            .map_err(|e| anyhow::anyhow!("failed to get AuthN resolver: {e}"))?;

        let gateway = ctx
            .client_hub()
            .get::<dyn ServiceGatewayClientV1>()
            .map_err(|e| anyhow::anyhow!("failed to get OAGW gateway: {e}"))?;

        // Pre-fill upstream_alias with host as fallback so ProviderResolver
        // works immediately. The actual OAGW registration is deferred to
        // start() because GTS instances are not visible via list() until
        // post_init (types-registry switches to ready mode there).
        for entry in cfg.providers.values_mut() {
            if entry.upstream_alias.is_none() {
                entry.upstream_alias = Some(entry.host.clone());
            }
            for ovr in entry.tenant_overrides.values_mut() {
                if ovr.upstream_alias.is_none()
                    && let Some(ref h) = ovr.host
                {
                    ovr.upstream_alias = Some(h.clone());
                }
            }
        }

        // Save a copy for deferred OAGW registration in start().
        // Ignore the result: if already set, we keep the first value.
        drop(self.oagw_deferred.set(OagwDeferred {
            gateway: Arc::clone(&gateway),
            authn: Arc::clone(&authn_client),
            client_credentials: cfg.client_credentials.clone(),
            providers: cfg.providers.clone(),
        }));

        let provider_resolver = Arc::new(ProviderResolver::new(&gateway, cfg.providers));

        let repos = Repositories {
            chat: Arc::new(ChatRepository::new(toolkit_db::odata::LimitCfg {
                default: 20,
                max: 100,
            })),
            attachment: Arc::new(AttachmentRepository),
            message: Arc::new(MessageRepository::new(toolkit_db::odata::LimitCfg {
                default: 20,
                max: 100,
            })),
            quota: Arc::new(QuotaUsageRepository),
            turn: Arc::new(TurnRepository),
            reaction: Arc::new(ReactionRepository),
            thread_summary: Arc::new(ThreadSummaryRepository),
            vector_store: Arc::new(VectorStoreRepository),
            message_attachment: Arc::new(MessageAttachmentRepository),
        };

        let rag_client = Arc::new(
            crate::infra::llm::providers::rag_http_client::RagHttpClient::new(Arc::clone(&gateway)),
        );

        // Build provider-specific file/vector store impls per provider entry.
        // Dispatch by storage_kind: Azure → Azure impls, OpenAi → OpenAI impls.
        let mut file_impls: std::collections::HashMap<
            String,
            Arc<dyn crate::domain::ports::FileStorageProvider>,
        > = std::collections::HashMap::new();
        let mut vs_impls: std::collections::HashMap<
            String,
            Arc<dyn crate::domain::ports::VectorStoreProvider>,
        > = std::collections::HashMap::new();
        for (provider_id, entry) in provider_resolver.entries() {
            let (file, vs): (
                Arc<dyn crate::domain::ports::FileStorageProvider>,
                Arc<dyn crate::domain::ports::VectorStoreProvider>,
            ) = match entry.storage_kind {
                crate::config::StorageKind::Azure => {
                    let api_version = entry.api_version.clone().unwrap_or_else(|| {
                        panic!(
                            "provider '{provider_id}': storage_kind is 'azure' \
                             but api_version is not set"
                        )
                    });
                    (
                        Arc::new(
                            crate::infra::llm::providers::azure_file_storage::AzureFileStorage::new(
                                Arc::clone(&rag_client),
                                Arc::clone(&provider_resolver),
                                api_version.clone(),
                            ),
                        ),
                        Arc::new(
                            crate::infra::llm::providers::azure_vector_store::AzureVectorStore::new(
                                Arc::clone(&rag_client),
                                Arc::clone(&provider_resolver),
                                api_version,
                            ),
                        ),
                    )
                }
                crate::config::StorageKind::OpenAi => (
                    Arc::new(
                        crate::infra::llm::providers::openai_file_storage::OpenAiFileStorage::new(
                            Arc::clone(&rag_client),
                            Arc::clone(&provider_resolver),
                        ),
                    ),
                    Arc::new(
                        crate::infra::llm::providers::openai_vector_store::OpenAiVectorStore::new(
                            Arc::clone(&rag_client),
                            Arc::clone(&provider_resolver),
                        ),
                    ),
                ),
            };
            file_impls.insert(provider_id.clone(), file);
            vs_impls.insert(provider_id.clone(), vs);
        }
        let file_storage: Arc<dyn crate::domain::ports::FileStorageProvider> = Arc::new(
            crate::infra::llm::providers::dispatching_storage::DispatchingFileStorage::new(
                file_impls,
            ),
        );
        let vector_store_prov: Arc<dyn crate::domain::ports::VectorStoreProvider> = Arc::new(
            crate::infra::llm::providers::dispatching_storage::DispatchingVectorStore::new(
                vs_impls,
            ),
        );

        // ── Metrics ─────────────────────────────────────────────────────────

        let metrics_prefix = cfg.metrics.effective_prefix(Self::MODULE_NAME);
        let scope =
            opentelemetry::InstrumentationScope::builder(Self::MODULE_NAME.to_owned()).build();
        let metrics: Arc<dyn MiniChatMetricsPort> = Arc::new(MiniChatMetricsMeter::new(
            &opentelemetry::global::meter_with_scope(scope),
            &metrics_prefix,
        ));

        // ── Outbox enqueuer (lazy) ────────────────────────────────────────
        //
        // The enqueuer is created now (services need it), but the actual outbox
        // pipeline starts in start() -- after OAGW upstreams are registered.
        // HTTP traffic doesn't arrive until after start(), so enqueue() is never
        // called before the outbox handle is set.

        let outbox_enqueuer = Arc::new(InfraOutboxEnqueuer::new(
            cfg.outbox.queue_name.clone(),
            cfg.outbox.cleanup_queue_name.clone(),
            cfg.outbox.chat_cleanup_queue_name.clone(),
            cfg.outbox.thread_summary_queue_name.clone(),
            cfg.outbox.audit_queue_name.clone(),
            cfg.outbox.num_partitions,
        ));

        // ── Knowledge retriever ─────────────────────────────────────────────

        let knowledge_retriever: Option<Arc<dyn crate::domain::ports::KnowledgeRetriever>> = if cfg
            .knowledge_search
            .enabled
        {
            Some(Arc::new(
                    crate::infra::llm::providers::azure_knowledge_retriever::AzureKnowledgeRetriever::new(
                        Arc::clone(&rag_client),
                    ),
                ))
        } else {
            None
        };

        // ── Anthropic Files API client ──────────────────────────────────────
        //
        // Constructed only when at least one provider entry uses the
        // `anthropic_messages` adapter — `AttachmentService` uses it for the
        // parallel "secondary" upload of attachments to Anthropic's Files API
        // (see `anthropic-provider-support.md` §8.0). The client itself is
        // upstream-agnostic; the upstream alias is passed per-call from the
        // resolved `UploadContext`. The same client is reused by the cleanup
        // worker to issue `DELETE /v1/files/{id}` on attachment / chat
        // deletion.
        let anthropic_files_client = if provider_resolver.entries().values().any(|entry| {
            matches!(
                entry.kind,
                crate::infra::llm::providers::ProviderKind::AnthropicMessages
            )
        }) {
            Some(Arc::new(
                crate::infra::llm::providers::anthropic_files_client::AnthropicFilesClient::new(
                    Arc::clone(&gateway),
                ),
            ))
        } else {
            None
        };

        // Save params for start() to build + start the outbox pipeline.
        drop(self.outbox_deferred.set(OutboxDeferred {
            db: Arc::clone(&db),
            outbox_config: cfg.outbox,
            cleanup_config: cfg.cleanup_worker,
            model_policy_gw: model_policy_gw.clone(),
            audit_gateway: Arc::clone(&audit_gateway),
            file_storage: Arc::clone(&file_storage),
            vector_store_prov: Arc::clone(&vector_store_prov),
            metrics: Arc::clone(&metrics),
            enqueuer: Arc::clone(&outbox_enqueuer),
            provider_resolver: Arc::clone(&provider_resolver),
            model_resolver: model_policy_gw.clone() as Arc<dyn crate::domain::repos::ModelResolver>,
            thread_summary_config: cfg.thread_summary_worker.clone(),
            anthropic_files_client: anthropic_files_client.clone(),
        }));

        // ── Services ────────────────────────────────────────────────────────

        let services = Arc::new(AppServices::new(
            &repos,
            db,
            authz,
            &(model_policy_gw.clone() as Arc<dyn crate::domain::repos::ModelResolver>),
            &provider_resolver,
            cfg.streaming,
            model_policy_gw.clone() as Arc<dyn crate::domain::repos::PolicySnapshotProvider>,
            model_policy_gw as Arc<dyn crate::domain::repos::UserLimitsProvider>,
            cfg.estimation_budgets,
            cfg.quota,
            &(outbox_enqueuer as Arc<dyn crate::domain::repos::OutboxEnqueuer>),
            cfg.context,
            file_storage,
            vector_store_prov,
            cfg.rag,
            cfg.thumbnail,
            metrics,
            cfg.thread_summary_worker,
            cfg.knowledge_search,
            knowledge_retriever,
            anthropic_files_client,
        ));

        self.service
            .set(services)
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        self.worker_configs
            .set(WorkerConfigs {
                orphan_watchdog: cfg.orphan_watchdog,
            })
            .map_err(|_| anyhow::anyhow!("{} worker_configs already set", Self::MODULE_NAME))?;

        info!("{} gear initialized successfully", Self::MODULE_NAME);
        Ok(())
    }
}

impl DatabaseCapability for MiniChatGear {
    fn migrations(&self) -> Vec<Box<dyn MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        info!("Providing mini-chat database migrations");
        let mut m = crate::infra::db::migrations::Migrator::migrations();
        m.extend(toolkit_db::outbox::outbox_migrations());
        m
    }
}

impl RestApiCapability for MiniChatGear {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        let services = self
            .service
            .get()
            .ok_or_else(|| anyhow::anyhow!("{} not initialized", Self::MODULE_NAME))?;

        info!("Registering mini-chat REST routes");
        let prefix = self
            .url_prefix
            .get()
            .ok_or_else(|| anyhow::anyhow!("{} not initialized (url_prefix)", Self::MODULE_NAME))?;

        let router = routes::register_routes(router, openapi, Arc::clone(services), prefix);
        info!("Mini-chat REST routes registered successfully");
        Ok(router)
    }
}

#[async_trait]
impl RunnableCapability for MiniChatGear {
    async fn start(&self, cancel: CancellationToken) -> anyhow::Result<()> {
        let wc = self.worker_configs.get().ok_or_else(|| {
            anyhow::anyhow!(
                "{} worker_configs not set - init() must run before start()",
                Self::MODULE_NAME
            )
        })?;
        let leader_elector = background_workers::prepare_worker_runtime(wc).await?;

        // Register OAGW upstreams now that GTS is in ready mode (post_init
        // has completed). During init() this fails because types-registry
        // list() only queries the persistent store which is empty until
        // switch_to_ready().
        if let Some(deferred) = self.oagw_deferred.get() {
            let ctx =
                exchange_client_credentials(&deferred.authn, &deferred.client_credentials).await?;
            let mut providers = deferred.providers.clone();
            let report = crate::infra::oagw_provisioning::register_oagw_upstreams(
                &deferred.gateway,
                &ctx,
                &mut providers,
            )
            .await?;
            // Fail-fast: a deterministically misconfigured provider (OAGW
            // rejected its request as invalid — retrying cannot help) aborts
            // startup instead of leaving the provider silently unavailable.
            // Only genuinely deferrable providers (secret not provisioned
            // yet, transient errors) proceed to the background retry below.
            report.ensure_no_misconfigured()?;
            let deferred_ids = report.deferred;

            // Providers whose backend secret was not accessible at boot are
            // registered lazily. With the stateful credstore, provider secrets
            // are created at runtime via the credstore API, so the upstream
            // cannot be registered until the secret exists. Retry in the
            // background rather than blocking startup: start() must return for
            // the server to report healthy, which is itself a precondition for
            // the secret to be provisioned.
            if !deferred_ids.is_empty() {
                tracing::warn!(
                    deferred = ?deferred_ids,
                    "OAGW upstreams deferred at boot (secret not yet accessible); \
                     retrying registration in the background"
                );
                let gateway = Arc::clone(&deferred.gateway);
                let authn = Arc::clone(&deferred.authn);
                let creds = deferred.client_credentials.clone();
                let providers = providers.clone();
                let cancel = cancel.clone();
                tokio::spawn(reconcile_deferred_upstreams_with_retry(
                    gateway,
                    authn,
                    creds,
                    providers,
                    deferred_ids,
                    cancel,
                ));
            }
        }

        // Start the outbox pipeline now that OAGW upstreams are registered.
        // Cleanup handlers can immediately call provider DELETE via OAGW.
        if let Some(od) = self.outbox_deferred.get() {
            let outbox_db = od.db.db();
            let num_partitions = od.outbox_config.num_partitions;
            let max_cleanup_attempts = od.cleanup_config.max_attempts;

            let partitions = Partitions::of(
                u16::try_from(num_partitions)
                    .map_err(|_| anyhow::anyhow!("num_partitions exceeds u16"))?,
            );

            let outbox_handle = Outbox::builder(outbox_db)
                .queue(&od.outbox_config.queue_name, partitions)
                .leased(UsageEventHandler {
                    plugin_provider: od.model_policy_gw.clone(),
                })
                .queue(&od.outbox_config.cleanup_queue_name, partitions)
                .leased(
                    crate::infra::workers::cleanup_worker::AttachmentCleanupHandler::new(
                        Arc::clone(&od.file_storage),
                        Arc::clone(&od.db),
                        ChatRepository::new(toolkit_db::odata::LimitCfg {
                            default: 20,
                            max: 100,
                        }),
                        max_cleanup_attempts,
                        Arc::clone(&od.metrics),
                        od.anthropic_files_client.clone(),
                    ),
                )
                .queue(&od.outbox_config.chat_cleanup_queue_name, partitions)
                .leased(
                    crate::infra::workers::cleanup_worker::ChatCleanupHandler::new(
                        Arc::clone(&od.file_storage),
                        Arc::clone(&od.vector_store_prov),
                        Arc::clone(&od.db),
                        ChatRepository::new(toolkit_db::odata::LimitCfg {
                            default: 20,
                            max: 100,
                        }),
                        max_cleanup_attempts,
                        Arc::clone(&od.metrics),
                        od.anthropic_files_client.clone(),
                    ),
                )
                .queue(&od.outbox_config.thread_summary_queue_name, partitions)
                .leased(
                    crate::infra::workers::thread_summary_worker::ThreadSummaryHandler::new(
                        Arc::new(
                            crate::infra::workers::thread_summary_worker::ThreadSummaryDeps {
                                db: Arc::clone(&od.db),
                                thread_summary_repo: Arc::new(ThreadSummaryRepository),
                                message_repo: Arc::new(MessageRepository::new(
                                    toolkit_db::odata::LimitCfg {
                                        default: 20,
                                        max: 100,
                                    },
                                )),
                                outbox_enqueuer: Arc::clone(&od.enqueuer)
                                    as Arc<dyn crate::domain::repos::OutboxEnqueuer>,
                                metrics: Arc::clone(&od.metrics),
                                provider_resolver: Arc::clone(&od.provider_resolver),
                                model_resolver: Arc::clone(&od.model_resolver),
                                config: od.thread_summary_config.clone(),
                            },
                        ),
                    ),
                )
                .queue(&od.outbox_config.audit_queue_name, partitions)
                .leased(AuditEventHandler {
                    audit_gateway: Arc::clone(&od.audit_gateway),
                    metrics: Arc::clone(&od.metrics),
                })
                .lease(LeaseConfig {
                    duration: Duration::from_mins(1),
                    ..LeaseConfig::default()
                })
                .start()
                .await
                .map_err(|e| anyhow::anyhow!("outbox start: {e}"))?;

            // Wire the outbox handle into the lazy enqueuer.
            od.enqueuer.set_outbox(Arc::clone(outbox_handle.outbox()));

            let mut guard = self
                .outbox_handle
                .lock()
                .map_err(|e| anyhow::anyhow!("outbox_handle lock: {e}"))?;
            *guard = Some(outbox_handle);

            info!("Outbox pipeline started (OAGW ready)");
        }

        let orphan_deps = if wc.orphan_watchdog.enabled {
            let services = self.service.get().ok_or_else(|| {
                anyhow::anyhow!(
                    "{} not initialized - init() must run before start()",
                    Self::MODULE_NAME
                )
            })?;
            Some(crate::infra::workers::orphan_watchdog::OrphanWatchdogDeps {
                finalization_svc: Arc::clone(&services.finalization),
                turn_repo: Arc::clone(&services.turn_repo),
                db: Arc::clone(&services.db),
                metrics: Arc::clone(&services.metrics),
            })
        } else {
            None
        };

        let (handles, worker_cancel) =
            background_workers::spawn_workers(wc, &cancel, leader_elector.as_ref(), orphan_deps)?;
        self.store_worker_runtime(handles, worker_cancel).await?;

        Ok(())
    }

    async fn stop(&self, cancel: CancellationToken) -> anyhow::Result<()> {
        if let Some(worker_cancel) = self
            .worker_cancel
            .lock()
            .map_err(|e| anyhow::anyhow!("worker_cancel lock: {e}"))?
            .take()
        {
            worker_cancel.cancel();
        }

        let workers = self
            .worker_handles
            .lock()
            .map_err(|e| anyhow::anyhow!("worker_handles lock: {e}"))?
            .take();
        if let Some(handles) = workers {
            info!("Waiting for background workers to stop");
            handles.join_all(cancel.clone(), WORKER_STOP_TIMEOUT).await;
            info!("Background workers stopped");
        }

        let handle = self
            .outbox_handle
            .lock()
            .map_err(|e| anyhow::anyhow!("outbox_handle lock: {e}"))?
            .take();
        if let Some(handle) = handle {
            info!("Stopping outbox pipeline");
            tokio::select! {
                () = handle.stop() => {
                    info!("Outbox pipeline stopped");
                }
                () = cancel.cancelled() => {
                    info!("Outbox pipeline stop cancelled by framework deadline");
                }
            }
        }
        Ok(())
    }
}

impl MiniChatGear {
    async fn store_worker_runtime(
        &self,
        handles: WorkerHandles,
        worker_cancel: CancellationToken,
    ) -> anyhow::Result<()> {
        let worker_cancel_cleanup = worker_cancel.clone();

        // Store cancel token. Guard must not live across an await point.
        let cancel_already_set = {
            let mut guard = self
                .worker_cancel
                .lock()
                .map_err(|e| anyhow::anyhow!("worker_cancel lock: {e}"))?;
            if guard.is_some() {
                true
            } else {
                *guard = Some(worker_cancel);
                false
            }
            // guard dropped here — before any await
        };
        if cancel_already_set {
            worker_cancel_cleanup.cancel();
            let hard_stop = CancellationToken::new();
            hard_stop.cancel();
            handles.join_all(hard_stop, WORKER_STOP_TIMEOUT).await;
            anyhow::bail!("{} worker_cancel already set", Self::MODULE_NAME);
        }

        // Store handles. Guard must not live across an await point.
        let mut handles = Some(handles);
        let handles_err = {
            match self.worker_handles.lock() {
                Ok(mut guard) => {
                    if guard.is_some() {
                        Some("worker_handles already set".to_owned())
                    } else {
                        *guard = handles.take();
                        None
                    }
                }
                Err(e) => Some(format!("worker_handles lock: {e}")),
            }
            // guard dropped here — before any await
        };
        if let Some(msg) = handles_err {
            if let Ok(mut cancel_guard) = self.worker_cancel.lock() {
                cancel_guard.take();
            }
            worker_cancel_cleanup.cancel();
            if let Some(handles) = handles {
                let hard_stop = CancellationToken::new();
                hard_stop.cancel();
                handles.join_all(hard_stop, WORKER_STOP_TIMEOUT).await;
            }
            // handles was either moved into the mutex (not the error case)
            // or never stored. In the "already set" case it was moved, so
            // we rely on the cancel token to stop workers; their JoinHandles
            // will be cleaned up when the existing WorkerHandles is joined
            // in stop().
            anyhow::bail!("{} {msg}", Self::MODULE_NAME);
        }
        Ok(())
    }
}

/// Background retry loop for providers whose OAGW upstream could not be
/// registered at boot because their credstore secret was not yet accessible.
///
/// Re-exchanges an S2S context and re-attempts registration on a backing-off
/// cadence until every deferred provider is registered or the gear is
/// cancelled. There is no attempt budget: with the stateful credstore a
/// provider's secret can be provisioned at any time after boot, and giving up
/// would leave that provider unavailable until an operator restart. The
/// interval grows from `RETRY_INTERVAL_MIN` to `RETRY_INTERVAL_MAX` so late
/// provisioning is still picked up without hammering OAGW indefinitely.
/// Registration is idempotent, so any provider registered on a previous
/// attempt is reused rather than duplicated.
// The retry loop's `select!`/tracing macros inflate the measured cognitive
// complexity; the control flow itself is a simple backing-off loop.
#[allow(clippy::cognitive_complexity)]
async fn reconcile_deferred_upstreams_with_retry(
    gateway: Arc<dyn ServiceGatewayClientV1>,
    authn: Arc<dyn AuthNResolverClient>,
    creds: crate::config::ClientCredentialsConfig,
    providers: std::collections::HashMap<String, ProviderEntry>,
    mut deferred: Vec<String>,
    cancel: CancellationToken,
) {
    const RETRY_INTERVAL_MIN: Duration = Duration::from_secs(2);
    const RETRY_INTERVAL_MAX: Duration = Duration::from_mins(1);
    // Warn once (not on every slow tick) after this much wall-clock time still
    // deferred, so an operator notices without log spam. Time-based, not
    // attempt-based: the interval backs off, so a fixed attempt count would
    // drift far from the intended window.
    const WARN_AFTER: Duration = Duration::from_mins(2);

    let started = tokio::time::Instant::now();
    let mut warned = false;
    let mut backoff = RETRY_INTERVAL_MIN;
    let mut attempt: u32 = 0;
    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                info!(remaining = deferred.len(), "OAGW upstream reconcile cancelled");
                return;
            }
            () = tokio::time::sleep(backoff) => {}
        }

        attempt = attempt.saturating_add(1);
        deferred =
            reconcile_deferred_once(&gateway, &authn, &creds, &providers, deferred, attempt).await;
        if deferred.is_empty() {
            info!("OAGW reconcile: all deferred provider upstreams registered");
            return;
        }

        if !warned && started.elapsed() >= WARN_AFTER {
            warned = true;
            tracing::warn!(
                remaining = ?deferred,
                elapsed_secs = started.elapsed().as_secs(),
                "OAGW reconcile: provider(s) still UNAVAILABLE; will keep retrying at a slower \
                 cadence until their secret is provisioned. Check for a missing secret or a \
                 misconfigured secret_ref/host for these providers"
            );
        }
        backoff = (backoff * 2).min(RETRY_INTERVAL_MAX);
    }
}

/// One reconcile attempt: exchange a fresh S2S context and re-register the
/// still-deferred providers. Returns the ids that remain deferred (the input
/// unchanged on a transient failure, so the caller keeps retrying).
// Two match arms plus tracing macros push the measured complexity over the
// threshold; the logic is linear.
#[allow(clippy::cognitive_complexity)]
async fn reconcile_deferred_once(
    gateway: &Arc<dyn ServiceGatewayClientV1>,
    authn: &Arc<dyn AuthNResolverClient>,
    creds: &crate::config::ClientCredentialsConfig,
    providers: &std::collections::HashMap<String, ProviderEntry>,
    deferred: Vec<String>,
    attempt: u32,
) -> Vec<String> {
    let ctx = match exchange_client_credentials(authn, creds).await {
        Ok(ctx) => ctx,
        Err(e) => {
            tracing::warn!(error = %e, attempt, "OAGW reconcile: credential exchange failed; will retry");
            return deferred;
        }
    };

    match crate::infra::oagw_provisioning::reconcile_deferred_upstreams(
        gateway, &ctx, providers, &deferred,
    )
    .await
    {
        Ok(still_deferred) => {
            let registered = deferred.len().saturating_sub(still_deferred.len());
            if registered > 0 {
                info!(
                    registered,
                    remaining = still_deferred.len(),
                    attempt,
                    "OAGW reconcile: registered deferred provider upstream(s)"
                );
            }
            still_deferred
        }
        Err(e) => {
            tracing::warn!(error = %e, attempt, "OAGW reconcile attempt failed; will retry");
            deferred
        }
    }
}

/// Exchange `OAuth2` client credentials via the `AuthN` resolver to obtain
/// a `SecurityContext` for OAGW upstream provisioning.
async fn exchange_client_credentials(
    authn: &Arc<dyn AuthNResolverClient>,
    creds: &crate::config::ClientCredentialsConfig,
) -> anyhow::Result<toolkit_security::SecurityContext> {
    info!("Exchanging client credentials for OAGW provisioning context");
    let request = ClientCredentialsRequest {
        client_id: creds.client_id.clone(),
        client_secret: creds.client_secret.clone(),
        scopes: Vec::new(),
    };
    let result = authn
        .exchange_client_credentials(&request)
        .await
        .map_err(|e| anyhow::anyhow!("client credentials exchange failed: {e}"))?;
    info!("Security context obtained for OAGW provisioning");
    Ok(result.security_context)
}
