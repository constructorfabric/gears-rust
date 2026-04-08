use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use authn_resolver_sdk::{AuthNResolverClient, ClientCredentialsRequest};
use authz_resolver_sdk::AuthZResolverClient;
use mini_chat_sdk::{MiniChatAuditPluginSpecV1, MiniChatModelPolicyPluginSpecV1};
use modkit::api::OpenApiRegistry;
use modkit::contracts::RunnableCapability;
use modkit::{DatabaseCapability, Module, ModuleCtx, RestApiCapability};
use modkit_db::outbox::{Outbox, OutboxHandle, Partitions};
use oagw_sdk::ServiceGatewayClientV1;
use sea_orm_migration::MigrationTrait;
use tokio_util::sync::CancellationToken;
use tracing::info;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use crate::api::rest::routes;
use crate::config::ProviderEntry;
use crate::domain::ports::MiniChatMetricsPort;
use crate::domain::service::{AppServices as GenericAppServices, Repositories};
use crate::infra::metrics::MiniChatMetricsMeter;
use crate::infra::outbox::{
    AttachmentCleanupHandler, AuditEventHandler, InfraOutboxEnqueuer, UsageEventHandler,
};

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

/// The mini-chat module: multi-tenant AI chat with SSE streaming.
#[modkit::module(
    name = "mini-chat",
    deps = ["types-registry", "authn-resolver", "authz-resolver", "oagw"],
    capabilities = [db, rest, stateful],
)]
pub struct MiniChatModule {
    service: OnceLock<Arc<AppServices>>,
    url_prefix: OnceLock<String>,
    outbox_handle: Mutex<Option<OutboxHandle>>,
    /// OAGW gateway + provider config for deferred upstream registration in `start()`.
    oagw_deferred: OnceLock<OagwDeferred>,
}

/// State needed to register OAGW upstreams in `start()` (after GTS is ready).
struct OagwDeferred {
    gateway: Arc<dyn ServiceGatewayClientV1>,
    authn: Arc<dyn AuthNResolverClient>,
    client_credentials: crate::config::ClientCredentialsConfig,
    providers: std::collections::HashMap<String, ProviderEntry>,
}

impl Default for MiniChatModule {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
            url_prefix: OnceLock::new(),
            outbox_handle: Mutex::new(None),
            oagw_deferred: OnceLock::new(),
        }
    }
}

#[allow(clippy::too_many_lines)]
#[async_trait]
impl Module for MiniChatModule {
    async fn init(&self, ctx: &ModuleCtx) -> anyhow::Result<()> {
        info!("Initializing {} module", Self::MODULE_NAME);

        let mut cfg: crate::config::MiniChatConfig = ctx.config_expanded()?;
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

        let vendor = cfg.vendor.trim().to_owned();
        if vendor.is_empty() {
            return Err(anyhow::anyhow!(
                "{}: vendor must be a non-empty string",
                Self::MODULE_NAME
            ));
        }

        let registry = ctx.client_hub().get::<dyn TypesRegistryClient>()?;
        register_plugin_schemas(
            &*registry,
            &[
                (
                    MiniChatModelPolicyPluginSpecV1::gts_schema_with_refs_as_string(),
                    MiniChatModelPolicyPluginSpecV1::gts_schema_id(),
                    "model-policy",
                ),
                (
                    MiniChatAuditPluginSpecV1::gts_schema_with_refs_as_string(),
                    MiniChatAuditPluginSpecV1::gts_schema_id(),
                    "audit",
                ),
            ],
        )
        .await?;

        self.url_prefix
            .set(cfg.url_prefix)
            .map_err(|_| anyhow::anyhow!("{} url_prefix already set", Self::MODULE_NAME))?;

        let db_provider = ctx.db_required()?;
        let db = Arc::new(db_provider);

        // Create the model-policy gateway early for both outbox handler and services.
        let model_policy_gw = Arc::new(ModelPolicyGateway::new(
            ctx.client_hub(),
            vendor.clone(),
            ctx.cancellation_token().clone(),
        ));

        // Audit gateway: lazily resolves audit plugin(s) on first emission.
        let audit_gateway = Arc::new(AuditGateway::new(ctx.client_hub(), vendor));

        // Start the outbox pipeline eagerly in init() (migrations ran in phase 2, DB is ready).
        // The framework guarantees stop() is called on init failure, so the pipeline
        // will be shut down cleanly if any later init step errors.
        // The handler resolves the plugin lazily on first message delivery,
        // avoiding a hard dependency on plugin availability during init().
        let outbox_db = db.db();
        let num_partitions = cfg.outbox.num_partitions;
        let queue_name = cfg.outbox.queue_name.clone();
        let cleanup_queue_name = cfg.outbox.cleanup_queue_name.clone();
        let audit_queue_name = cfg.outbox.audit_queue_name.clone();

        let partitions = Partitions::of(
            u16::try_from(num_partitions)
                .map_err(|_| anyhow::anyhow!("num_partitions {num_partitions} exceeds u16"))?,
        );

        // Metrics are created here (before the outbox) so they can be passed to AuditEventHandler.
        let metrics_prefix = cfg.metrics.effective_prefix(Self::MODULE_NAME);
        let scope =
            opentelemetry::InstrumentationScope::builder(Self::MODULE_NAME.to_owned()).build();
        let metrics: Arc<dyn MiniChatMetricsPort> = Arc::new(MiniChatMetricsMeter::new(
            &opentelemetry::global::meter_with_scope(scope),
            &metrics_prefix,
        ));

        let outbox_handle = Outbox::builder(outbox_db)
            .queue(&queue_name, partitions)
            .decoupled(UsageEventHandler {
                plugin_provider: model_policy_gw.clone(),
            })
            .queue(&cleanup_queue_name, partitions)
            .decoupled(AttachmentCleanupHandler)
            .queue(&audit_queue_name, partitions)
            .decoupled(AuditEventHandler {
                audit_gateway: Arc::clone(&audit_gateway),
                metrics: Arc::clone(&metrics),
            })
            .start()
            .await
            .map_err(|e| anyhow::anyhow!("outbox start: {e}"))?;

        let outbox = Arc::clone(outbox_handle.outbox());
        let outbox_enqueuer = Arc::new(InfraOutboxEnqueuer::new(
            outbox,
            queue_name,
            cleanup_queue_name,
            audit_queue_name,
            num_partitions,
        ));

        {
            let mut guard = self
                .outbox_handle
                .lock()
                .map_err(|e| anyhow::anyhow!("outbox_handle lock: {e}"))?;
            if guard.is_some() {
                return Err(anyhow::anyhow!(
                    "{} outbox_handle already set",
                    Self::MODULE_NAME
                ));
            }
            *guard = Some(outbox_handle);
        }

        info!("Outbox pipeline started");

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
            chat: Arc::new(ChatRepository::new(modkit_db::odata::LimitCfg {
                default: 20,
                max: 100,
            })),
            attachment: Arc::new(AttachmentRepository),
            message: Arc::new(MessageRepository::new(modkit_db::odata::LimitCfg {
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
            metrics,
        ));

        self.service
            .set(services)
            .map_err(|_| anyhow::anyhow!("{} module already initialized", Self::MODULE_NAME))?;

        info!("{} module initialized successfully", Self::MODULE_NAME);
        Ok(())
    }
}

impl DatabaseCapability for MiniChatModule {
    fn migrations(&self) -> Vec<Box<dyn MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        info!("Providing mini-chat database migrations");
        let mut m = crate::infra::db::migrations::Migrator::migrations();
        m.extend(modkit_db::outbox::outbox_migrations());
        m
    }
}

impl RestApiCapability for MiniChatModule {
    fn register_rest(
        &self,
        _ctx: &ModuleCtx,
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
impl RunnableCapability for MiniChatModule {
    async fn start(&self, _cancel: CancellationToken) -> anyhow::Result<()> {
        // Register OAGW upstreams now that GTS is in ready mode (post_init
        // has completed). During init() this fails because types-registry
        // list() only queries the persistent store which is empty until
        // switch_to_ready().
        if let Some(deferred) = self.oagw_deferred.get() {
            let ctx =
                exchange_client_credentials(&deferred.authn, &deferred.client_credentials).await?;
            let mut providers = deferred.providers.clone();
            crate::infra::oagw_provisioning::register_oagw_upstreams(
                &deferred.gateway,
                &ctx,
                &mut providers,
            )
            .await?;
        }
        Ok(())
    }

    async fn stop(&self, cancel: CancellationToken) -> anyhow::Result<()> {
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

/// Exchange `OAuth2` client credentials via the `AuthN` resolver to obtain
/// a `SecurityContext` for OAGW upstream provisioning.
async fn exchange_client_credentials(
    authn: &Arc<dyn AuthNResolverClient>,
    creds: &crate::config::ClientCredentialsConfig,
) -> anyhow::Result<modkit_security::SecurityContext> {
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

async fn register_plugin_schemas(
    registry: &dyn TypesRegistryClient,
    schemas: &[(String, &str, &str)],
) -> anyhow::Result<()> {
    let mut payload = Vec::with_capacity(schemas.len());
    for (schema_str, schema_id, _label) in schemas {
        let mut schema_json: serde_json::Value = serde_json::from_str(schema_str)?;
        let obj = schema_json
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("schema {schema_id} is not a JSON object"))?;
        obj.insert(
            "additionalProperties".to_owned(),
            serde_json::Value::Bool(false),
        );
        payload.push(schema_json);
    }
    let results = registry.register(payload).await?;
    RegisterResult::ensure_all_ok(&results)?;
    for (_schema_str, schema_id, label) in schemas {
        info!(schema_id = %schema_id, "Registered {label} plugin schema in types-registry");
    }
    Ok(())
}
