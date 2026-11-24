//! Fail-closed gear bootstrap (`features/foundation.md`, "Gear Bootstrap and
//! Readiness").
//!
//! Runs in the lifecycle entry before the ready signal. Every step that fails
//! records the failing dependency in [`Readiness`], so the health endpoint
//! names it, and returns an error so the runtime never marks the gear ready.
//! The lifecycle entry runs it again while the error is a dependency that
//! does not answer. It reads the metric types of the active Quotas and
//! Policies into the catalogue: a registry that does not answer that read
//! fails the run, a wrong metric type does not (ADR-0012). The notifications feature ends it by resolving
//! the notification sinks and starting delivery, so events flow before the
//! gear is ready.

use std::sync::Arc;

use quota_enforcement_sdk::{
    BootstrapBundle, EnvironmentInputs, MetricId, PolicyDraft, PolicySchemaSnapshot, PolicyScope,
    QuotaEnforcementStoragePluginV1, StorageError,
};
use toolkit_macros::domain_model;

use super::catalog::MetricCatalog;
use super::engines::{EngineRegistry, PolicyArtifactCache, linked_registry};
use super::error::{Dependency, DomainError};
use super::notifications::{DeliveryLifecycle, DispatchLimits, NotificationDispatcher};
use super::plugins::PluginBinding;
use super::policies::PolicyRuntimeLimits;
use super::ports::coordination::{CoordinatorBinding, SingletonCoordinator};
use super::ports::metrics::EngineLabel;
use super::ports::metrics::QeMetrics;
use super::ports::pdp::PdpProbe;
use super::readiness::Readiness;

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
    /// The metric catalogue, filled while the gear runs.
    pub catalog: Arc<MetricCatalog>,
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
    catalog: Arc<MetricCatalog>,
    metrics: Arc<dyn QeMetrics>,
    readiness: Arc<Readiness>,
    limits: PolicyRuntimeLimits,
    dispatch: DispatchLimits,
    delivery: Arc<DeliveryLifecycle>,
}

impl Bootstrap {
    /// Assemble the procedure.
    #[must_use]
    pub fn new(
        binding: PluginBinding,
        coordinator: Arc<dyn CoordinatorBinding>,
        pdp: Arc<dyn PdpProbe>,
        catalog: Arc<MetricCatalog>,
        reporting: BootstrapReporting,
        limits: PolicyRuntimeLimits,
    ) -> Self {
        Self {
            binding,
            coordinator,
            pdp,
            catalog,
            metrics: reporting.metrics,
            readiness: reporting.readiness,
            limits,
            dispatch: DispatchLimits::default(),
            delivery: Arc::default(),
        }
    }

    /// Dispatch notifications within the operator's `limits` instead of the
    /// defaults.
    #[must_use]
    pub const fn with_dispatch(mut self, limits: DispatchLimits) -> Self {
        self.dispatch = limits;
        self
    }

    /// The notification pipeline a successful run started; the lifecycle
    /// entry stops it on shutdown.
    #[must_use]
    pub fn delivery(&self) -> Arc<DeliveryLifecycle> {
        Arc::clone(&self.delivery)
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

    /// Run the bootstrap steps, naming the dependency that failed.
    // @cpt-algo:cpt-cf-quota-enforcement-algo-engine-bootstrap-seed:p1
    async fn run_steps(&self) -> Result<Bound, (Dependency, DomainError)> {
        // @cpt-begin:cpt-cf-quota-enforcement-algo-engine-bootstrap-seed:p1:inst-ebs-register
        let EngineRuntime { engines, artifacts } = self.engine_runtime()?;
        // @cpt-end:cpt-cf-quota-enforcement-algo-engine-bootstrap-seed:p1:inst-ebs-register

        // @cpt-begin:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-start
        // Resolve the configured storage implementation.
        let storage = self
            .binding
            .resolve_storage()
            .await
            .map_err(|e| (Dependency::Storage, e))?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-start

        // @cpt-begin:cpt-cf-quota-enforcement-algo-engine-bootstrap-seed:p1:inst-ebs-seed
        // @cpt-begin:cpt-cf-quota-enforcement-algo-engine-bootstrap-seed:p1:inst-ebs-order
        // Engine registration precedes policy seeding.
        let mut bundle = BootstrapBundle::foundation();
        bundle.global_policy = Some(global_policy_seed());
        storage
            .bootstrap(&bundle)
            .await
            .map_err(|e| (Dependency::Storage, lift_bootstrap_storage_error(e)))?;
        // @cpt-end:cpt-cf-quota-enforcement-algo-engine-bootstrap-seed:p1:inst-ebs-order
        // @cpt-end:cpt-cf-quota-enforcement-algo-engine-bootstrap-seed:p1:inst-ebs-seed

        // @cpt-begin:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-cluster-resolve
        // Resolution validates the profile and linearizable-election support.
        let coordinator = self
            .coordinator
            .resolve()
            .await
            .map_err(|e| (Dependency::Cluster, e))?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-cluster-resolve

        // @cpt-begin:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-pdp-probe
        // Registration alone does not prove that the PDP is reachable.
        self.pdp.probe().await.map_err(|e| (Dependency::Pdp, e))?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-gear-bootstrap:p1:inst-boot-pdp-probe

        let policy_metrics = self
            .publish_active_policies(storage.as_ref(), &engines, &artifacts)
            .await?;
        self.read_catalog_at_start(storage.as_ref(), policy_metrics)
            .await?;
        self.start_delivery(storage.as_ref()).await?;

        Ok(Bound {
            engines,
            artifacts,
            storage,
            coordinator,
            catalog: Arc::clone(&self.catalog),
        })
    }

    /// The statically linked engines and the bounded artifact cache.
    ///
    /// Built before any policy is seeded or scanned, so no active policy can
    /// ever name an engine this binary does not link. A duplicate registration
    /// is a readiness failure, never an overwrite, and it is counted against
    /// the engine that collided.
    fn engine_runtime(&self) -> Result<EngineRuntime, (Dependency, DomainError)> {
        let engines = linked_registry().map_err(|e| {
            // @cpt-begin:cpt-cf-quota-enforcement-algo-engine-bootstrap-seed:p1:inst-ebs-fail-if
            // @cpt-begin:cpt-cf-quota-enforcement-algo-engine-bootstrap-seed:p1:inst-ebs-fail
            self.metrics
                .record_engine_bootstrap_failure(EngineLabel::from_linked_id(e.engine_id));
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

    /// Resolve every notification sink and start the storage plugin's
    /// notification pipeline over a dispatcher to them. The last step: no
    /// later failure can leave a pipeline running behind a failed bootstrap.
    // @cpt-flow:cpt-cf-quota-enforcement-flow-sink-delivery:p1
    async fn start_delivery(
        &self,
        storage: &dyn QuotaEnforcementStoragePluginV1,
    ) -> Result<(), (Dependency, DomainError)> {
        let sinks = self.binding.resolve_sinks().await.map_err(|e| match e {
            DomainError::TypesRegistryUnavailable(_) => (Dependency::TypesRegistry, e),
            other => (Dependency::NotificationSinks, other),
        })?;
        // @cpt-begin:cpt-cf-quota-enforcement-flow-sink-delivery:p1:inst-del-nosink-if
        if sinks.is_empty() {
            // @cpt-begin:cpt-cf-quota-enforcement-flow-sink-delivery:p1:inst-del-nosink-warn
            tracing::warn!(
                target: LOG_TARGET,
                "no notification sinks registered; events are acknowledged undelivered"
            );
            // @cpt-end:cpt-cf-quota-enforcement-flow-sink-delivery:p1:inst-del-nosink-warn
        }
        // @cpt-end:cpt-cf-quota-enforcement-flow-sink-delivery:p1:inst-del-nosink-if
        let dispatcher =
            NotificationDispatcher::new(sinks, self.dispatch, Arc::clone(&self.metrics));
        let handle = storage
            .start_notification_delivery(Arc::new(dispatcher))
            .await
            .map_err(|e| (Dependency::Storage, DomainError::from(e)))?;
        if let Some(earlier) = self.delivery.hold(handle) {
            // An earlier run's pipeline: stopped, never leaked.
            earlier.stop().await;
        }
        Ok(())
    }

    /// Rebuild every active policy's immutable artifact from its persisted
    /// config and schema snapshot, refusing readiness for one this deployment
    /// cannot support. Nothing here falls back to another engine: a policy
    /// naming an engine that is not registered fails the gear, not the policy.
    /// A policy is not checked against the current metric type: a mismatch is
    /// found at evaluation (ADR-0012). Returns the metric types the active
    /// policies name.
    async fn publish_active_policies(
        &self,
        storage: &dyn QuotaEnforcementStoragePluginV1,
        engines: &EngineRegistry,
        artifacts: &PolicyArtifactCache,
    ) -> Result<Vec<MetricId>, (Dependency, DomainError)> {
        let active = storage
            .read_active_policies()
            .await
            .map_err(|e| (Dependency::Storage, e.into()))?;
        let mut metrics = Vec::new();
        for version in active {
            if let PolicyScope::Metric { metric } = &version.scope {
                metrics.push(metric.clone());
            }
            metrics.extend(
                version
                    .schema_snapshot
                    .schemas
                    .keys()
                    .filter_map(|raw| MetricCatalog::parse_metric(raw)),
            );
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
            let artifact = engine
                .validate_config(quota_enforcement_sdk::EngineValidationInput {
                    raw: &version.engine_config,
                    schemas: &version.schema_snapshot,
                })
                .map_err(|e| {
                    self.metrics
                        .record_engine_bootstrap_failure(EngineLabel::of(engine.as_ref()));
                    (Dependency::Engine, DomainError::Internal(e.to_string()))
                })?;
            artifacts.publish(version.policy_id, version.version, artifact);
        }
        Ok(metrics)
    }

    /// Fill the catalogue with the metric types of the active Quotas and of
    /// `policy_metrics`. A failed read fails the run, so a replica does not
    /// take traffic with an empty catalogue; a metric type that is not valid
    /// does not.
    async fn read_catalog_at_start(
        &self,
        storage: &dyn QuotaEnforcementStoragePluginV1,
        mut metrics: Vec<MetricId>,
    ) -> Result<(), (Dependency, DomainError)> {
        let counts = storage
            .read_active_quota_counts()
            .await
            .map_err(|e| (Dependency::Storage, DomainError::from(e)))?;
        metrics.extend(counts.by_metric.into_keys());
        self.catalog
            .read_at_start(&metrics)
            .await
            .map_err(|e| (Dependency::TypesRegistry, e))
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
