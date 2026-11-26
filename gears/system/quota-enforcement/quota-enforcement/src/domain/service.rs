//! The gear's domain service: admission plus the dependencies bootstrap binds.
//!
//! It builds every component per call over admission in one of its two
//! modes: PDP mode for QE's API, bound to the caller's `SecurityContext`, and
//! trusted mode for the quota resolver plugin (ADR-0009).

use std::sync::{Arc, OnceLock};

use quota_enforcement_sdk::QuotaEnforcementStoragePluginV1;
use toolkit_macros::domain_model;
use toolkit_security::{PlatformIdentity, SecurityContext};

use super::admission::{Admission, Gate};
use super::bootstrap::Bound;
use super::catalog::MetricCatalog;
use super::error::{Dependency, DomainError};
use super::policies::PolicyRuntimeLimits;
use super::ports::coordination::SingletonCoordinator;
use super::quotas::{QuotaLimits, QuotaManagement};
use super::readiness::Readiness;

/// Process-local runtime bounds of the hot path, passed at construction
/// because they are neither PDP state nor bootstrap dependencies.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct OperationsRuntime {
    /// Replay records the cache holds.
    pub cache_entries: usize,
    /// How long a cached record may answer.
    pub cache_ttl: std::time::Duration,
    /// How many preparations one operation may trigger.
    pub preparation_max_attempts: std::num::NonZeroU32,
    /// The TTL window lease acquisitions are checked against.
    pub leases: super::operations::LeaseLimits,
    /// The batch debit's item limit and timeout.
    pub batch: super::operations::BatchLimits,
    /// The snapshot read's page size and subject bound.
    pub snapshot: super::operations::SnapshotLimits,
}

/// Composition root of the domain. Handlers and the in-process client reach
/// every dependency through it.
#[domain_model]
pub struct Service {
    admission: Admission,
    readiness: Arc<Readiness>,
    limits: QuotaLimits,
    policy_limits: PolicyRuntimeLimits,
    bound: OnceLock<Bound>,
    policy_schemas: OnceLock<super::policies::schemas::CatalogPolicySchemas>,
    operations: super::operations::IdempotencyCache,
    evaluation: quota_enforcement_sdk::engine::EvaluationLimits,
    preparation_max_attempts: std::num::NonZeroU32,
    leases: super::operations::LeaseLimits,
    batch: super::operations::BatchLimits,
    snapshot: super::operations::SnapshotLimits,
}

impl Service {
    /// Assemble the service. Dependencies are bound later by bootstrap.
    #[must_use]
    pub fn new(
        admission: Admission,
        readiness: Arc<Readiness>,
        limits: QuotaLimits,
        policy_limits: PolicyRuntimeLimits,
        operations: OperationsRuntime,
    ) -> Self {
        Self {
            admission,
            readiness,
            limits,
            policy_limits,
            bound: OnceLock::new(),
            policy_schemas: OnceLock::new(),
            operations: super::operations::IdempotencyCache::new(
                operations.cache_entries,
                operations.cache_ttl,
            ),
            evaluation: policy_limits.evaluation,
            preparation_max_attempts: operations.preparation_max_attempts,
            leases: operations.leases,
            batch: operations.batch,
            snapshot: operations.snapshot,
        }
    }

    /// The PEP boundary.
    #[must_use]
    pub fn admission(&self) -> &Admission {
        &self.admission
    }

    /// Readiness cell shared with the health check.
    #[must_use]
    pub fn readiness(&self) -> &Arc<Readiness> {
        &self.readiness
    }

    /// Publish the bootstrapped dependencies. Set once.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::Internal`] when dependencies were bound before.
    pub fn bind(&self, bound: Bound) -> Result<(), DomainError> {
        self.policy_schemas
            .set(super::policies::schemas::CatalogPolicySchemas::new(
                Arc::clone(&bound.catalog),
                self.policy_limits.snapshot,
            ))
            .map_err(|_| DomainError::Internal("dependencies were already bound".into()))?;
        self.bound
            .set(bound)
            .map_err(|_| DomainError::Internal("dependencies were already bound".to_owned()))
    }

    /// The active storage plugin.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::NotReady`] before bootstrap completed.
    pub fn storage(&self) -> Result<Arc<dyn QuotaEnforcementStoragePluginV1>, DomainError> {
        self.bound
            .get()
            .map(|b| b.storage.clone())
            .ok_or(DomainError::NotReady {
                dependency: Dependency::Storage,
            })
    }

    /// The sweeper coordinator over the platform `cluster` gear.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::NotReady`] before bootstrap completed.
    pub fn coordinator(&self) -> Result<Arc<dyn SingletonCoordinator>, DomainError> {
        self.bound
            .get()
            .map(|b| b.coordinator.clone())
            .ok_or(DomainError::NotReady {
                dependency: Dependency::Cluster,
            })
    }

    /// The metric catalogue.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::NotReady`] before bootstrap completed.
    pub fn catalog(&self) -> Result<Arc<MetricCatalog>, DomainError> {
        self.bound
            .get()
            .map(|b| b.catalog.clone())
            .ok_or(DomainError::NotReady {
                dependency: Dependency::Catalog,
            })
    }

    /// The shared operator policy lifecycle for one call of QE's API.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::NotReady`] before bootstrap publishes the dependencies.
    pub fn policies<'a>(
        &'a self,
        ctx: &'a SecurityContext,
    ) -> Result<super::policies::PolicyManagement<'a>, DomainError> {
        self.policies_over(self.admission.pdp(ctx))
    }

    fn policies_over<'a>(
        &'a self,
        admission: Gate<'a>,
    ) -> Result<super::policies::PolicyManagement<'a>, DomainError> {
        let bound = self.bound.get().ok_or(DomainError::NotReady {
            dependency: Dependency::Storage,
        })?;
        let schemas = self.policy_schemas.get().ok_or(DomainError::NotReady {
            dependency: Dependency::Catalog,
        })?;
        Ok(super::policies::PolicyManagement {
            admission,
            storage: bound.storage.as_ref(),
            engines: &bound.engines,
            cache: &bound.artifacts,
            schemas,
            metrics: self.admission.metrics(),
            limits: self.policy_limits.authoring,
        })
    }

    /// The consumption hot path for one call of QE's API, over PDP admission:
    /// debit, credit, rollback, preview, batch, snapshot, and leases.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::NotReady`] before bootstrap completed.
    pub fn operations<'a>(
        &'a self,
        ctx: &'a SecurityContext,
    ) -> Result<super::operations::Operations<'a>, DomainError> {
        self.operations_over(self.admission.pdp(ctx))
    }

    /// The same operations over trusted admission, for one call through the
    /// quota resolver plugin (ADR-0009).
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::NotReady`] before bootstrap completed.
    pub(crate) fn trusted_operations(
        &self,
        caller: Option<PlatformIdentity>,
    ) -> Result<super::operations::Operations<'_>, DomainError> {
        self.operations_over(self.admission.trusted(caller))
    }

    fn operations_over<'a>(
        &'a self,
        admission: Gate<'a>,
    ) -> Result<super::operations::Operations<'a>, DomainError> {
        let bound = self.bound.get().ok_or(DomainError::NotReady {
            dependency: Dependency::Storage,
        })?;
        Ok(super::operations::Operations {
            admission,
            catalog: &bound.catalog,
            storage: bound.storage.as_ref(),
            engines: Arc::clone(&bound.engines),
            artifacts: Arc::clone(&bound.artifacts),
            idempotency: &self.operations,
            metrics: self.admission.metrics_handle(),
            evaluation: self.evaluation,
            preparation_max_attempts: self.preparation_max_attempts,
            leases: self.leases,
            batch: self.batch,
            snapshot: self.snapshot,
        })
    }

    /// The Quota lifecycle for one call of QE's API: create, update,
    /// deactivate, read.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::NotReady`] before bootstrap completed.
    pub fn quotas<'a>(
        &'a self,
        ctx: &'a SecurityContext,
    ) -> Result<QuotaManagement<'a>, DomainError> {
        let bound = self.bound.get().ok_or(DomainError::NotReady {
            dependency: Dependency::Storage,
        })?;
        Ok(QuotaManagement::new(
            self.admission.pdp(ctx),
            &bound.catalog,
            bound.storage.as_ref(),
            self.admission.metrics(),
            self.limits,
        ))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "service_tests.rs"]
mod service_tests;
