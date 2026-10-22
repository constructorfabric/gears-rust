//! The policy-engine gear: wiring and readiness.
//!
//! `init` validates the configuration, resolves the types-registry,
//! tenant-resolver and authz-resolver clients, validates and compiles the
//! built-in policies (any finding fails startup), builds the decision and
//! management services, registers `dyn PolicyManagementClientV1` in
//! `ClientHub`, registers the admission engine plugin in the types registry
//! and scopes `dyn AdmissionEnginePluginClientV1` to its instance id.
//!
//! The types registry answers type lookups only once it is ready, after
//! every `init`: `init` validates the built-ins in everything but whether
//! their concrete resource types are known, and `post_init` (system
//! capability, after the registry's own) completes that check, failing
//! startup on a finding. Nothing is served before `post_init`. The gear runs
//! no background tasks; it is ready once initialised.

use std::sync::{Arc, OnceLock};

use admission_control_sdk::AdmissionEnginePluginClientV1;
use anyhow::Context as _;
use async_trait::async_trait;
use authz_resolver_sdk::{AuthZResolverApi, PolicyEnforcer};
use policy_engine_sdk::PolicyManagementClientV1;
use sea_orm_migration::MigratorTrait;
use tenant_resolver_sdk::TenantResolverClient;
use toolkit::api::OpenApiRegistry;
use toolkit::client_hub::ClientScope;
use toolkit::contracts::{DatabaseCapability, SystemCapability};
use toolkit::{ClientHub, Gear, GearCtx, Healthcheck, HealthcheckResult, RestApiCapability};
use toolkit_db::Db;
use toolkit_db::odata::LimitCfg;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};

use crate::config::PolicyEngineConfig;
use crate::domain::decision::{BuiltinPolicySet, CompileCache, DecisionMetrics, DecisionService};
use crate::domain::engine_plugin::{self, PolicyEngineAdmissionPlugin};
use crate::domain::management::{
    ManagementService, ManagementServiceParts, PolicyManagementLocalClient, StoreSet,
};
use crate::domain::model::Document;
use crate::domain::ports::{HierarchyPort, TypeCatalogPort};
use crate::domain::validation::{ContentValidator, Finding};
use crate::infra::binding_source::DbBindingSource;
use crate::infra::metrics::PolicyEngineMetrics;
use crate::infra::storage::content_repo::{
    OrmAssignmentRepository, OrmBundleRepository, OrmVersionRepository,
};
use crate::infra::tenant_hierarchy::TenantResolverHierarchy;
use crate::infra::types_registry::RegistryTypeCatalog;

/// Name of the readiness check.
pub const READINESS_CHECK_NAME: &str = "policy-engine-readiness";

const BUNDLE_LIST_DEFAULT_PAGE: u64 = 25;
const BUNDLE_LIST_MAX_PAGE: u64 = 100;

type Store = StoreSet<OrmBundleRepository, OrmVersionRepository, OrmAssignmentRepository>;

struct Wired {
    management_client: Arc<dyn PolicyManagementClientV1>,
    /// The built-in policies, for the registry check of `post_init`.
    builtins: Vec<Document>,
    validator: Arc<ContentValidator>,
}

/// The policy-engine gear.
#[toolkit::gear(
    name = "policy-engine",
    deps = [types_registry, tenant_resolver, authz_resolver],
    capabilities = [system, db, rest]
)]
#[derive(Default)]
pub struct PolicyEngine {
    wired: OnceLock<Wired>,
}

impl std::fmt::Debug for PolicyEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PolicyEngine")
            .field("initialised", &self.wired.get().is_some())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl Gear for PolicyEngine {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let config: PolicyEngineConfig = ctx
            .config_or_default()
            .context("policy-engine: invalid configuration")?;
        config
            .validate()
            .context("policy-engine: configuration rejected")?;

        let db = ctx
            .db()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "policy-engine: no database bound; add `gears.policy-engine.database`"
                )
            })?
            .db();
        let clients = Clients::resolve(ctx)?;
        let types: Arc<dyn TypeCatalogPort> = Arc::new(RegistryTypeCatalog::new(
            Arc::clone(&clients.types_registry),
            config.registry_timeout,
        ));
        let validator = Arc::new(ContentValidator::new(types, config.content_limits()));
        let documents = config.builtin_documents();
        let builtins = load_builtins(&validator, &documents)
            .context("policy-engine: built-in policies rejected")?;
        let builtin_count = builtins.len();
        let (decision, management_client) =
            build_services(&config, &db, &clients, Arc::clone(&validator), builtins);
        register_clients_and_plugin(&clients, &config, &decision, &management_client).await?;

        self.wired
            .set(Wired {
                management_client,
                builtins: documents,
                validator,
            })
            .map_err(|_| anyhow::anyhow!("{} gear already initialized", Self::MODULE_NAME))?;
        tracing::info!(
            builtin_policies = builtin_count,
            engine_vendor = config.engine_plugin.vendor,
            engine_priority = config.engine_plugin.priority,
            "policy-engine initialised"
        );
        Ok(())
    }
}

struct Clients {
    hub: Arc<ClientHub>,
    types_registry: Arc<dyn TypesRegistryClient>,
    tenant_resolver: Arc<dyn TenantResolverClient>,
    authz: Arc<dyn AuthZResolverApi>,
}

impl Clients {
    fn resolve(ctx: &GearCtx) -> anyhow::Result<Self> {
        let hub = ctx.client_hub();
        let types_registry = hub.get::<dyn TypesRegistryClient>().map_err(|e| {
            anyhow::anyhow!("policy-engine: types-registry client unavailable: {e}")
        })?;
        let tenant_resolver = hub.get::<dyn TenantResolverClient>().map_err(|e| {
            anyhow::anyhow!("policy-engine: tenant-resolver client unavailable: {e}")
        })?;
        let authz = hub.get::<dyn AuthZResolverApi>().map_err(|e| {
            anyhow::anyhow!("policy-engine: authz-resolver client unavailable: {e}")
        })?;
        Ok(Self {
            hub,
            types_registry,
            tenant_resolver,
            authz,
        })
    }
}

#[async_trait]
impl SystemCapability for PolicyEngine {
    /// Completes the built-ins' validation with the types registry, which
    /// answers type lookups once its own `post_init` has run (it is a
    /// dependency, so it runs first).
    async fn post_init(&self, _sys: &toolkit::runtime::SystemContext) -> anyhow::Result<()> {
        let wired = self
            .wired
            .get()
            .ok_or_else(|| anyhow::anyhow!("{}: not initialised", Self::MODULE_NAME))?;
        if wired.builtins.is_empty() {
            return Ok(());
        }
        let findings = wired
            .validator
            .validate(&wired.builtins)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "policy-engine: built-in policies rejected: types registry could not be consulted: {e}"
                )
            })?;
        reject_findings(&findings).context("policy-engine: built-in policies rejected")
    }
}

/// Validates the configured built-in policies like a bundle version's
/// documents, except for the types registry check `post_init` completes,
/// then compiles them.
fn load_builtins(
    validator: &ContentValidator,
    documents: &[Document],
) -> anyhow::Result<BuiltinPolicySet> {
    if documents.is_empty() {
        return Ok(BuiltinPolicySet::default());
    }
    reject_findings(&validator.validate_offline(documents))?;
    Ok(BuiltinPolicySet::compile(documents)?)
}

/// An error naming every finding, if any.
fn reject_findings(findings: &[Finding]) -> anyhow::Result<()> {
    if findings.is_empty() {
        return Ok(());
    }
    let findings: Vec<String> = findings
        .iter()
        .map(|f| match &f.document_name {
            Some(id) => format!("`{id}`: {} ({})", f.message, f.code),
            None => format!("{} ({})", f.message, f.code),
        })
        .collect();
    anyhow::bail!("{}", findings.join("; "))
}

fn build_services(
    config: &PolicyEngineConfig,
    db: &Db,
    clients: &Clients,
    validator: Arc<ContentValidator>,
    builtins: BuiltinPolicySet,
) -> (Arc<DecisionService>, Arc<dyn PolicyManagementClientV1>) {
    let hierarchy: Arc<dyn HierarchyPort> = Arc::new(TenantResolverHierarchy::new(
        Arc::clone(&clients.tenant_resolver),
        config.hierarchy_timeout,
    ));

    let decision = Arc::new(DecisionService::new(
        builtins,
        Arc::clone(&hierarchy),
        Arc::new(DbBindingSource::new(db.clone(), config.store_timeout)),
        Arc::new(CompileCache::new(config.compile_cache_capacity)),
        config.evaluation_timeout,
        Arc::new(PolicyEngineMetrics::global()) as Arc<dyn DecisionMetrics>,
    ));

    let store: Arc<Store> = Arc::new(StoreSet {
        bundles: OrmBundleRepository::new(LimitCfg {
            default: BUNDLE_LIST_DEFAULT_PAGE,
            max: BUNDLE_LIST_MAX_PAGE,
        }),
        versions: OrmVersionRepository,
        assignments: OrmAssignmentRepository,
    });
    let management = Arc::new(ManagementService::new(ManagementServiceParts {
        db: db.clone(),
        store,
        enforcer: PolicyEnforcer::new(Arc::clone(&clients.authz)),
        validator,
        hierarchy,
    }));
    let management_client: Arc<dyn PolicyManagementClientV1> =
        Arc::new(PolicyManagementLocalClient::new(management));
    (decision, management_client)
}

async fn register_clients_and_plugin(
    clients: &Clients,
    config: &PolicyEngineConfig,
    decision: &Arc<DecisionService>,
    management_client: &Arc<dyn PolicyManagementClientV1>,
) -> anyhow::Result<()> {
    clients
        .hub
        .register::<dyn PolicyManagementClientV1>(Arc::clone(management_client));

    let (plugin_instance_id, plugin_json) =
        engine_plugin::registration(&config.engine_plugin.vendor, config.engine_plugin.priority)
            .map_err(|e| {
                anyhow::anyhow!("policy-engine: engine plugin registration could not be built: {e}")
            })?;
    let results = clients
        .types_registry
        .register(vec![plugin_json])
        .await
        .map_err(|e| anyhow::anyhow!("policy-engine: engine plugin registration failed: {e}"))?;
    RegisterResult::ensure_all_ok(&results)
        .map_err(|e| anyhow::anyhow!("policy-engine: engine plugin registration rejected: {e}"))?;
    clients
        .hub
        .register_scoped::<dyn AdmissionEnginePluginClientV1>(
            ClientScope::gts_id(&plugin_instance_id),
            Arc::new(PolicyEngineAdmissionPlugin::new(Arc::clone(decision))),
        );
    Ok(())
}

impl RestApiCapability for PolicyEngine {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        let wired = self
            .wired
            .get()
            .ok_or_else(|| anyhow::anyhow!("{}: not initialised", Self::MODULE_NAME))?;
        Ok(crate::api::rest::register_routes(
            router,
            openapi,
            Arc::clone(&wired.management_client),
        ))
    }

    fn healthcheck(&self, _ctx: &GearCtx) -> Option<Arc<dyn Healthcheck>> {
        Some(Arc::new(PolicyEngineReadiness {
            initialised: self.wired.get().is_some(),
        }))
    }
}

impl DatabaseCapability for PolicyEngine {
    fn migrations(&self) -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        crate::infra::storage::Migrator::migrations()
    }
}

struct PolicyEngineReadiness {
    initialised: bool,
}

#[async_trait]
impl Healthcheck for PolicyEngineReadiness {
    fn name(&self) -> &'static str {
        READINESS_CHECK_NAME
    }

    async fn check(&self) -> HealthcheckResult {
        if self.initialised {
            HealthcheckResult::healthy()
        } else {
            HealthcheckResult::unhealthy("policy-engine is not initialised")
                .with_code("not_initialised")
        }
    }
}
