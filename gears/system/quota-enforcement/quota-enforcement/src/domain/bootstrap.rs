//! Fail-closed gear bootstrap (`features/foundation.md`, "Gear Bootstrap and
//! Readiness").
//!
//! Runs in the lifecycle entry before the ready signal. Every step that fails
//! records the failing dependency in [`Readiness`], so the health endpoint
//! names it, and returns an error so the runtime never marks the gear ready.
//! The projection-contracts feature adds the QE-owned GTS registration, the
//! catalogue consistency set, and the compatibility check against active
//! Quotas (`features/projection-contracts.md`). The quota-lifecycle feature
//! adds the removed-metric scan over the bound metrics and binds the registries
//! the write path needs. Later features extend [`Bootstrap::run`] with their
//! own steps.

use std::sync::Arc;

use quota_enforcement_sdk::{
    BootstrapBundle, EnvironmentInputs, PolicyDraft, PolicySchemaSnapshot, PolicyScope,
    QuotaEnforcementStoragePluginV1, StorageError, owned_definitions,
};
use toolkit_macros::domain_model;

use super::catalog::{CatalogBuilder, CatalogConfig, ProjectionContractCatalog};
use super::engines::{EngineRegistry, PolicyArtifactCache, builtin_registry};
use super::error::{Dependency, DomainError};
use super::plugins::PluginBinding;
use super::policies::PolicyRuntimeLimits;
use super::policies::PolicySchemas;
use super::policies::schemas::CatalogPolicySchemas;
use super::ports::contracts::ContractRegistry;
use super::ports::coordination::{CoordinatorBinding, SingletonCoordinator};
use super::ports::metric_registry::MetricRegistry;
use super::ports::metrics::EngineLabel;
use super::ports::metrics::QeMetrics;
use super::ports::pdp::PdpProbe;
use super::readiness::Readiness;
use quota_enforcement_sdk::MetricId;

const LOG_TARGET: &str = "qe.bootstrap";

/// Dependencies bound by a successful bootstrap.
#[domain_model]
#[derive(Clone)]
pub struct Bound {
    /// Statically linked engines registered before policy seeding.
    pub engines: Arc<super::engines::EngineRegistry>,
    /// Bounded immutable artifacts, never active pointers.
    pub artifacts: Arc<super::engines::PolicyArtifactCache>,

    /// The active storage plugin.
    pub storage: Arc<dyn QuotaEnforcementStoragePluginV1>,
    /// The sweeper coordinator over the platform `cluster` gear.
    pub coordinator: Arc<dyn SingletonCoordinator>,
    /// The published projection contract catalogue. Immutable for the process.
    pub catalog: Arc<ProjectionContractCatalog>,
    /// The contract registry the write path snapshots projections from.
    pub registry: Arc<dyn ContractRegistry>,
    /// The metric identity and classification registry. The Quota and Policy
    /// write paths read it; the evaluation path never does.
    pub metric_registry: Arc<dyn MetricRegistry>,
    /// Classification of every admitted metric, frozen here so the evaluation
    /// path answers from process memory.
    pub classifications: Arc<super::catalog::MetricClassifications>,
}

/// The registry the catalogue is built from and the projections to build it
/// for.
#[domain_model]
pub struct CatalogBinding {
    /// The contract registry (platform `types-registry`).
    pub registry: Arc<dyn ContractRegistry>,
    /// The configured projections.
    pub config: CatalogConfig,
}

/// The engine side of a bootstrap: what was registered and where its compiled
/// artifacts live. Built before any policy is seeded or scanned.
struct EngineRuntime {
    engines: Arc<EngineRegistry>,
    artifacts: Arc<PolicyArtifactCache>,
}

/// The two channels a bootstrap reports its outcome on: the readiness cell the
/// health endpoint reads, and the metrics a failed step increments.
#[domain_model]
pub struct BootstrapReporting {
    /// Instrumentation for bootstrap failures.
    pub metrics: Arc<dyn QeMetrics>,
    /// Readiness cell shared with the health check.
    pub readiness: Arc<Readiness>,
}

/// The bootstrap procedure.
#[domain_model]
pub struct Bootstrap {
    binding: PluginBinding,
    coordinator: Arc<dyn CoordinatorBinding>,
    pdp: Arc<dyn PdpProbe>,
    catalog: CatalogBinding,
    metric_registry: Arc<dyn MetricRegistry>,
    metrics: Arc<dyn QeMetrics>,
    readiness: Arc<Readiness>,
    limits: PolicyRuntimeLimits,
}

impl Bootstrap {
    /// Assemble the procedure.
    #[must_use]
    pub fn new(
        binding: PluginBinding,
        coordinator: Arc<dyn CoordinatorBinding>,
        pdp: Arc<dyn PdpProbe>,
        catalog: CatalogBinding,
        metric_registry: Arc<dyn MetricRegistry>,
        reporting: BootstrapReporting,
        limits: PolicyRuntimeLimits,
    ) -> Self {
        Self {
            binding,
            coordinator,
            pdp,
            catalog,
            metric_registry,
            metrics: reporting.metrics,
            readiness: reporting.readiness,
            limits,
        }
    }

    /// Run every step. On success the readiness cell is `Ready`; on failure
    /// it names the dependency and the error is returned.
    ///
    /// # Errors
    ///
    /// The first failing step's [`DomainError`].
    // @cpt-flow:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1
    pub async fn run(&self) -> Result<Bound, DomainError> {
        match self.run_steps().await {
            Ok(bound) => {
                // @cpt-begin:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-ready
                // @cpt-begin:cpt-cf-quota-enforcement-algo-engine-bootstrap-seed:p1:inst-ebs-return
                self.readiness.mark_ready();
                // @cpt-end:cpt-cf-quota-enforcement-algo-engine-bootstrap-seed:p1:inst-ebs-return
                tracing::info!(target: LOG_TARGET, "quota enforcement bootstrap complete");
                Ok(bound)
                // @cpt-end:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-ready
            }
            // @cpt-begin:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-probe-if
            Err((dependency, err)) => {
                // @cpt-begin:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-probe-abort
                self.readiness.mark_failed(dependency, err.to_string());
                tracing::error!(
                    target: LOG_TARGET,
                    dependency = %dependency,
                    error = %err,
                    "quota enforcement bootstrap failed; the gear serves nothing"
                );
                Err(err)
                // @cpt-end:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-probe-abort
            } // @cpt-end:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-probe-if
        }
    }

    // @cpt-algo:cpt-cf-quota-enforcement-algo-engine-bootstrap-seed:p1
    async fn run_steps(&self) -> Result<Bound, (Dependency, DomainError)> {
        // @cpt-begin:cpt-cf-quota-enforcement-algo-engine-bootstrap-seed:p1:inst-ebs-register
        let EngineRuntime { engines, artifacts } = self.engine_runtime()?;
        // @cpt-end:cpt-cf-quota-enforcement-algo-engine-bootstrap-seed:p1:inst-ebs-register

        // @cpt-begin:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-start
        // Exactly one active storage plugin: the instance the configured vendor
        // selects.
        let storage = self
            .binding
            .resolve_storage()
            .await
            .map_err(|e| (Dependency::Storage, e))?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-start

        // Schema check and default seeding are the plugin's steps.
        // @cpt-begin:cpt-cf-quota-enforcement-algo-engine-bootstrap-seed:p1:inst-ebs-seed
        // @cpt-begin:cpt-cf-quota-enforcement-algo-engine-bootstrap-seed:p1:inst-ebs-order
        // Engines are registered above, before this seed runs, so the seeded
        // policy can never name an engine this binary does not link.
        let mut bundle = BootstrapBundle::foundation();
        bundle.global_policy = Some(global_policy_seed());
        storage
            .bootstrap(&bundle)
            .await
            .map_err(|e| (Dependency::Storage, lift_bootstrap_storage_error(e)))?;
        // @cpt-end:cpt-cf-quota-enforcement-algo-engine-bootstrap-seed:p1:inst-ebs-order
        // @cpt-end:cpt-cf-quota-enforcement-algo-engine-bootstrap-seed:p1:inst-ebs-seed

        // @cpt-begin:cpt-cf-quota-enforcement-flow-owner-projection-publication:p1:inst-pub-boot
        // @cpt-begin:cpt-cf-quota-enforcement-algo-catalog-bootstrap:p1:inst-cat-bases
        // The QE-owned GTS definitions: the four abstract bases, the scope
        // type, and its two well-known instances. Idempotent; a byte-identical
        // definition already present is a success, a different one a conflict.
        let definitions = owned_definitions().map_err(|e| {
            (
                Dependency::Catalog,
                DomainError::Internal(format!("embedded GTS definition does not parse: {e}")),
            )
        })?;
        self.catalog
            .registry
            .ensure_registered(&definitions)
            .await
            .map_err(|e| (catalog_dependency(&e), e))?;
        // @cpt-end:cpt-cf-quota-enforcement-algo-catalog-bootstrap:p1:inst-cat-bases

        // The consistency set over the configured projections, then the
        // compatibility check against the Quotas storage holds. The catalogue
        // is published only after both pass; a failure serves nothing.
        let builder = CatalogBuilder::new(self.catalog.registry.as_ref(), self.metrics.as_ref());
        let catalog = builder
            .build(&self.catalog.config)
            .await
            .map_err(|e| (catalog_dependency(&e), e))?;
        let bindings = storage
            .read_active_projection_bindings()
            .await
            .map_err(|e| (Dependency::Storage, DomainError::from(e)))?;
        builder
            .check_compatibility(&catalog, &bindings)
            .map_err(|e| (Dependency::Catalog, e))?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-owner-projection-publication:p1:inst-pub-boot

        // One pass over the registry for every metric this deployment can see:
        // those the catalogue admits, so the evaluation path never has to call
        // it, and those active Quotas are bound to, so a Quota stranded on a
        // removed metric is flagged. Each distinct metric is looked up once, and
        // a registry that does not answer fails readiness.
        // Collected first: the lookup awaits, and holding a borrow of the
        // catalogue across it would tie this future to their lifetimes.
        let to_classify: Vec<MetricId> = catalog
            .admitted_metrics()
            .cloned()
            .chain(bindings.iter().map(|binding| binding.metric.clone()))
            .collect();
        let classifications = Arc::new(
            super::catalog::MetricClassifications::load(to_classify, self.metric_registry.as_ref())
                .await
                .map_err(|e| (Dependency::TypesRegistry, e))?,
        );

        // @cpt-begin:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-cluster-resolve
        // The cluster resolver validates the operator's binding of the
        // `quota-enforcement` profile: an unbound profile or a backend without a
        // linearizable election fails here. There is no probe of our own.
        let coordinator = self
            .coordinator
            .resolve()
            .await
            .map_err(|e| (Dependency::Cluster, e))?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-cluster-resolve

        // @cpt-begin:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-pdp-probe
        // One round trip to the PDP. `init` already proved the client is
        // registered; a registered client over an unreachable PDP would still
        // deny every request, so the gear must not report ready behind it.
        self.pdp.probe().await.map_err(|e| (Dependency::Pdp, e))?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-pdp-probe

        let catalog = Arc::new(catalog);
        let schemas = CatalogPolicySchemas::new(Arc::clone(&catalog), self.limits.snapshot);
        self.publish_active_policies(storage.as_ref(), &engines, &artifacts, &schemas)
            .await?;

        Ok(Bound {
            engines,
            artifacts,
            storage,
            coordinator,
            catalog,
            registry: self.catalog.registry.clone(),
            metric_registry: self.metric_registry.clone(),
            classifications,
        })
    }

    /// The statically linked engines and the bounded artifact cache.
    ///
    /// Built before any policy is seeded or scanned, so no active policy can
    /// ever name an engine this binary does not link. A duplicate registration
    /// is a readiness failure, never an overwrite, and it is counted against
    /// the engine that collided.
    fn engine_runtime(&self) -> Result<EngineRuntime, (Dependency, DomainError)> {
        let engines = builtin_registry().map_err(|e| {
            // @cpt-begin:cpt-cf-quota-enforcement-algo-engine-bootstrap-seed:p1:inst-ebs-fail-if
            // @cpt-begin:cpt-cf-quota-enforcement-algo-engine-bootstrap-seed:p1:inst-ebs-fail
            if let Some(label) = EngineLabel::from_id(e.engine_id) {
                self.metrics.record_engine_bootstrap_failure(label);
            }
            tracing::error!(
                target: LOG_TARGET,
                engine_id = e.engine_id,
                "engine registration failed; refusing to serve"
            );
            (Dependency::Engine, DomainError::Internal(e.to_string()))
            // @cpt-end:cpt-cf-quota-enforcement-algo-engine-bootstrap-seed:p1:inst-ebs-fail
            // @cpt-end:cpt-cf-quota-enforcement-algo-engine-bootstrap-seed:p1:inst-ebs-fail-if
        })?;
        let artifacts = PolicyArtifactCache::new(
            self.limits.artifact_cache_entries,
            self.limits.preparation_max_concurrency,
        );
        Ok(EngineRuntime {
            engines: Arc::new(engines),
            artifacts: Arc::new(artifacts),
        })
    }

    /// Rebuild every active policy's immutable artifact from its persisted
    /// config and schema snapshot, refusing readiness for one this deployment
    /// cannot support. Nothing here falls back to another engine: a policy
    /// naming an engine that is not registered fails the gear, not the policy.
    async fn publish_active_policies(
        &self,
        storage: &dyn QuotaEnforcementStoragePluginV1,
        engines: &EngineRegistry,
        artifacts: &PolicyArtifactCache,
        schemas: &CatalogPolicySchemas,
    ) -> Result<(), (Dependency, DomainError)> {
        let active = storage
            .read_active_policies()
            .await
            .map_err(|e| (Dependency::Storage, e.into()))?;
        for version in active {
            let engine = engines.get(&version.engine_id).ok_or_else(|| {
                tracing::error!(
                    target: LOG_TARGET,
                    policy_id = %version.policy_id,
                    engine_id = %version.engine_id,
                    "active policy names an engine this deployment does not register"
                );
                (
                    Dependency::Engine,
                    DomainError::InvalidPolicy {
                        field: "engine_id",
                        reason: "UNKNOWN_ENGINE",
                        detail: "active policy engine is not registered".into(),
                    },
                )
            })?;
            schemas
                .check_activation(&version)
                .map_err(|e| (Dependency::Catalog, e))?;
            let artifact = engine
                .validate_config(quota_enforcement_sdk::EngineValidationInput {
                    raw: &version.engine_config,
                    schemas: &version.schema_snapshot,
                })
                .map_err(|e| {
                    if let Some(label) = EngineLabel::from_id(engine.id()) {
                        self.metrics.record_engine_bootstrap_failure(label);
                    }
                    (Dependency::Engine, DomainError::Internal(e.to_string()))
                })?;
            artifacts.publish(version.policy_id, version.version, artifact);
        }
        Ok(())
    }
}

/// The policy every deployment starts with: `most-restrictive-wins`, version 1,
/// empty config. Seeded idempotently; an operator's later change survives a
/// restart because the plugin re-reads the scope before writing.
fn global_policy_seed() -> PolicyDraft {
    PolicyDraft {
        scope: PolicyScope::Global,
        engine_id: "most-restrictive-wins".into(),
        engine_config: serde_json::json!({}),
        timeout_ms: None,
        description: Some("platform default resolution policy".into()),
        comment: Some("seeded at bootstrap".into()),
        // Overwritten by storage with the system actor; nothing to attribute.
        created_by: String::new(),
        schema_snapshot: PolicySchemaSnapshot {
            inputs: EnvironmentInputs::NONE,
            ..PolicySchemaSnapshot::default()
        },
    }
}

/// A catalogue failure is the catalogue's fault when the registry answered
/// and a consistency check failed, and the registry's when it did not answer.
fn catalog_dependency(err: &DomainError) -> Dependency {
    match err {
        DomainError::CatalogInvalid { .. } => Dependency::Catalog,
        _ => Dependency::TypesRegistry,
    }
}

/// At bootstrap a schema mismatch is a named, fatal condition rather than
/// the generic internal error the runtime lift produces.
fn lift_bootstrap_storage_error(err: StorageError) -> DomainError {
    match err {
        StorageError::SchemaVersionMismatch {
            installed,
            expected,
        } => DomainError::SchemaVersionMismatch {
            installed,
            expected,
        },
        other => DomainError::from(other),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "bootstrap_tests.rs"]
mod bootstrap_tests;
