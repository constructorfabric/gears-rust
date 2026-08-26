//! Composition root: probes the server, selects the store and engine
//! implementations, and publishes the in-process client.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use authz_resolver_sdk::pep::PolicyEnforcer;
use toolkit::api::OpenApiRegistry;
use toolkit::{DatabaseCapability, Gear, GearCtx, RestApiCapability};
use tracing::{debug, info, warn};

use graph_storage_sdk::GraphStorageClientV1;

use crate::api::rest::routes;
use crate::config::GraphStorageConfig;
use crate::domain::local_client::GraphStorageLocalClient;
use crate::domain::service::GraphServices;
use crate::infra::engine::PgGraphEngine;
use crate::infra::store::PgGraphStore;

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
        let pgq_available = pgq_expected();
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

        let services = Arc::new(GraphServices::new(cfg, store, engine, enforcer));
        self.services
            .set(Arc::clone(&services))
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;

        ctx.client_hub()
            .register::<dyn GraphStorageClientV1>(Arc::new(GraphStorageLocalClient::new(services)));

        info!(pgq_available, "graph-storage gear initialized");
        Ok(())
    }
}

/// Whether this deployment is expected to serve `GRAPH_TABLE`.
///
/// **Not a probe.** The readiness matrix asks for the server major, but a gear
/// cannot issue a catalog query: the sealed runner exposes no statement API,
/// deliberately. The migration already made the real decision — it created
/// the property graph only when the major allowed — so the engine assumes the
/// capability and falls back per request, with a logged reason, when a
/// pattern does not run. See `dev/DEVIATIONS.md` D-004.
const fn pgq_expected() -> bool {
    true
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
