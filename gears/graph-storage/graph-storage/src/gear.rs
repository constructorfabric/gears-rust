//! Composition root: probes the server, selects the store and engine
//! implementations, and publishes the in-process client.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use authz_resolver_sdk::pep::PolicyEnforcer;
use toolkit::api::OpenApiRegistry;
use toolkit::{DatabaseCapability, Gear, GearCtx, RestApiCapability};
use tracing::{debug, error, info, warn};

use graph_storage_sdk::GraphStorageClientV1;
use graph_storage_sdk::plugin_api::EmbeddingProviderV1;

use crate::api::rest::routes;
use crate::config::GraphStorageConfig;
use crate::domain::local_client::GraphStorageLocalClient;
use crate::domain::service::GraphServices;
use crate::domain::embedding::SpaceState;
use crate::infra::embedding::fake::FakeEmbeddingProvider;
use crate::infra::engine::PgGraphEngine;
use crate::infra::store::{PgGraphStore, spaces};

/// The graph-storage gear.
#[toolkit::gear(name = "graph-storage", deps = [authz_resolver], capabilities = [db, rest])]
pub struct GraphStorage {
    services: OnceLock<Arc<GraphServices>>,
}

impl Default for GraphStorage {
    fn default() -> Self {
        Self {
            services: OnceLock::new(),
        }
    }
}

#[async_trait]
impl Gear for GraphStorage {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: GraphStorageConfig = ctx.config_or_default()?;
        cfg.validate()?;
        debug!(
            traversal_hop = ?cfg.traversal_hop,
            ingest_max_nodes = cfg.ingest_max_nodes,
            "loaded graph-storage configuration"
        );

        // The configured vector width must be the width the schema was
        // migrated with, or every stored vector is incomparable with every
        // query vector.
        let migrated = crate::infra::store::ingest::migrated_embedding_dimension();
        if cfg.embedding_dimension != migrated {
            anyhow::bail!(
                "graph-storage.embedding_dimension is {} but the schema was migrated with {migrated}; \
                 vector search would compare incomparable vectors",
                cfg.embedding_dimension
            );
        }

        // Acquiring the database capability is what makes the platform run
        // this gear's migrations before the REST phase; declaring `db` alone
        // is silently insufficient.
        let db_raw = ctx.db_required()?;
        let db = Arc::new(db_raw.db());

        // SQL/PGQ is a probed backend capability, not a gear requirement:
        // the property-graph migration is skipped on an older server, and the
        // engine then serves every hop on the fallback backend.
        let pgq_available = crate::infra::engine::probe_pgq(&db).await;
        if !pgq_available {
            warn!("this server does not provide SQL/PGQ; traversal will use the two-query hop");
        }

        let store = Arc::new(PgGraphStore::new(
            Arc::clone(&db),
            cfg.clone(),
            pgq_available,
        ));
        let engine = Arc::new(PgGraphEngine::new(Arc::clone(&store)));
        let enforcer = PolicyEnforcer::new(ctx.client_hub().get()?);

        let provider = select_embedding_provider(&cfg);
        let embedding = resolve_embedding_space(&db, provider, &cfg).await?;

        let services = Arc::new(GraphServices::new(cfg, store, engine, enforcer, embedding));
        self.services
            .set(Arc::clone(&services))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        ctx.client_hub()
            .register::<dyn GraphStorageClientV1>(Arc::new(GraphStorageLocalClient::new(services)));

        info!(pgq_available, "graph-storage gear initialized");
        Ok(())
    }
}

/// Pick the deployment's one embedding provider.
///
/// One per deployment, per the single-embedding-space constraint. The
/// in-process ONNX provider ADR-0005 makes the default lives in its own crate
/// and registers itself; what this composition root can always build is the
/// deterministic fake, which is also the only sensible choice when a
/// deployment has declared no model.
fn select_embedding_provider(cfg: &GraphStorageConfig) -> Arc<dyn EmbeddingProviderV1> {
    warn!(
        "no embedding provider is configured; falling back to the deterministic fake. \
         Vector search will answer, but its ranking carries no semantics"
    );
    Arc::new(FakeEmbeddingProvider::new(cfg.embedding_dimension))
}

/// Reconcile the provider against the space the stored vectors belong to.
///
/// A mismatch does not stop the gear: only the vector arm is incomparable, and
/// every other path serves the same rows it always did. It stops *that arm*,
/// loudly, which is what `fr-embedding-dim-guard` asks for — the readiness
/// surface that should also report it does not exist yet (`dev/DEVIATIONS.md`).
async fn resolve_embedding_space(
    db: &toolkit_db::secure::Db,
    provider: Arc<dyn EmbeddingProviderV1>,
    cfg: &GraphStorageConfig,
) -> anyhow::Result<crate::domain::embedding::EmbeddingCoordinator> {
    // The provider's own width against the migrated column, before anything
    // is written: a provider of the wrong width cannot produce one storable
    // vector, so this is a configuration error rather than a runtime one.
    if provider.dimension() != cfg.embedding_dimension {
        anyhow::bail!(
            "the embedding provider declares {} dimensions but \
             graph-storage.embedding_dimension is {}",
            provider.dimension(),
            cfg.embedding_dimension
        );
    }

    let state = match spaces::resolve(db, provider.embedding_space()).await? {
        spaces::SpaceResolution::Active { epoch } => {
            info!(
                epoch,
                identity = %provider.embedding_space().identity_hash,
                model = %provider.embedding_space().model_artifact,
                "embedding space active"
            );
            SpaceState::Active { epoch }
        }
        spaces::SpaceResolution::Mismatched {
            recorded_identity,
            recorded_epoch,
        } => {
            error!(
                recorded_epoch,
                recorded_identity = %recorded_identity,
                active_identity = %provider.embedding_space().identity_hash,
                "stored vectors belong to a different embedding space than the configured \
                 provider; vector search is blocked until the graph is re-embedded"
            );
            SpaceState::Blocked
        }
    };

    Ok(crate::domain::embedding::EmbeddingCoordinator::new(
        provider,
        state,
        cfg.embedding_input_max_bytes,
    ))
}

impl DatabaseCapability for GraphStorage {
    fn migrations(&self) -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        use sea_orm_migration::MigratorTrait;
        crate::infra::storage::migrations::Migrator::migrations()
    }
}

impl RestApiCapability for GraphStorage {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        let services = self
            .services
            .get()
            .ok_or_else(|| anyhow::anyhow!("graph-storage services are not initialized"))?
            .clone();
        Ok(routes::register_routes(router, openapi, services))
    }
}
